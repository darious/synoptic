//! `/ss_webstream_task/?method=MixStream…`: the live (`stmSrc=0`) and
//! history (`stmSrc=2`) WebSocket streams, speaking exactly what
//! `ss_stream` parses:
//!
//! - binary frames: 4-byte big-endian header length, ASCII `k=v&…` header
//!   (`mediaType`, `msec`, `vdoCodec`, `adoCodec`, `adoExtra`, `close`),
//!   payload (an access unit without its leading start code, or a
//!   prefixed AAC frame);
//! - text from the client: `keepAlive`, and on history connections the
//!   in-band `action=play&…`, `seekMs=…&stamp=n`, `pause=…`, `speed=…`
//!   commands.
//!
//! Authentication is the `Cookie: id=<sid>` upgrade header only.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::time::{Instant, sleep_until};

use crate::api::{App, Params};
use crate::media::{Media, VIDEO_FRAME_MS};
use crate::world::{self, CameraDef};

/// DSM drops a connection whose client has been silent this long.
pub const CLIENT_SILENCE_TIMEOUT: Duration = Duration::from_secs(90);
/// What every stream here advertises.
pub const VIDEO_CODEC: &str = "H264";
pub const AUDIO_CODEC: &str = "MPEG4-GENERIC";

static CONN_SEQ: AtomicU64 = AtomicU64::new(0);

/// Build one wire frame.
pub fn encode_frame(header: &str, payload: &[u8]) -> Bytes {
    let mut out = Vec::with_capacity(4 + header.len() + payload.len());
    out.extend_from_slice(&(header.len() as u32).to_be_bytes());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(payload);
    Bytes::from(out)
}

fn cookie_sid(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .find_map(|c| c.strip_prefix("id="))
        .map(str::to_owned)
}

enum Kind {
    Live {
        camera_id: i64,
        profile: i64,
        audio: bool,
    },
    History,
}

/// `GET /ss_webstream_task/`.
pub async fn ws_handler(
    State(app): State<Arc<App>>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let mut params = Params::default();
    params.parse_into(query.unwrap_or_default().as_bytes());
    if !cookie_sid(&headers).is_some_and(|sid| app.is_session(&sid)) {
        tracing::warn!("ws rejected: no valid session cookie");
        return (StatusCode::UNAUTHORIZED, "no session").into_response();
    }
    if params.get("method") != Some("MixStream") {
        return (StatusCode::BAD_REQUEST, "method=MixStream required").into_response();
    }
    let kind = match params.get("stmSrc") {
        Some("0") => Kind::Live {
            camera_id: params.int("id").unwrap_or(0),
            profile: params.int("profile").unwrap_or(1),
            audio: params.boolean("blAudio").unwrap_or(true),
        },
        Some("2") => Kind::History,
        other => {
            tracing::warn!(stm_src = other, "ws rejected: unsupported stmSrc");
            return (StatusCode::BAD_REQUEST, "unsupported stmSrc").into_response();
        }
    };
    let conn = CONN_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    ws.on_upgrade(move |socket| run(app, socket, conn, kind))
}

async fn run(app: Arc<App>, socket: WebSocket, conn: u64, kind: Kind) {
    let (tx, rx) = socket.split();
    let media = Arc::clone(&app.media);
    let mode = match kind {
        Kind::Live {
            camera_id,
            profile,
            audio,
        } => match app.world.camera(camera_id) {
            Some(camera) if camera.enabled() => {
                tracing::info!(
                    conn,
                    camera = camera.name,
                    camera_id,
                    profile,
                    audio,
                    "ws live stream opened"
                );
                Mode::Live(Live {
                    camera,
                    audio: audio && camera.audio,
                    start: Instant::now(),
                    next_v: 0,
                    next_a: 0,
                })
            }
            Some(camera) => {
                tracing::warn!(
                    conn,
                    camera = camera.name,
                    "ws live stream for a disabled camera"
                );
                Mode::Dead
            }
            None => {
                tracing::warn!(conn, camera_id, "ws live stream for an unknown camera");
                Mode::Dead
            }
        },
        Kind::History => {
            tracing::info!(conn, "ws history stream opened; waiting for action=play");
            Mode::History(None)
        }
    };
    let mut s = Streamer {
        app,
        media,
        conn,
        tx,
        mode,
        sent_video: 0,
        sent_audio: 0,
        last_client_msg: Instant::now(),
    };
    let why = s.run(rx).await;
    tracing::info!(
        conn,
        video = s.sent_video,
        audio = s.sent_audio,
        "ws closed: {why}"
    );
}

/// Where history playback is, in recording milliseconds, as a function
/// of wall-clock time.
struct Clock {
    anchor_pos: f64,
    anchor_at: Instant,
    speed: f64,
    reverse: bool,
    paused: bool,
}

impl Clock {
    fn pos_at(&self, now: Instant) -> f64 {
        if self.paused {
            return self.anchor_pos;
        }
        let elapsed = now.saturating_duration_since(self.anchor_at).as_secs_f64() * 1000.0;
        let delta = elapsed * self.speed;
        if self.reverse {
            (self.anchor_pos - delta).max(0.0)
        } else {
            self.anchor_pos + delta
        }
    }

    fn anchor(&mut self, pos: f64, now: Instant) {
        self.anchor_pos = pos;
        self.anchor_at = now;
    }

    /// When position `pos` is due.
    fn wall_for(&self, pos: f64) -> Instant {
        let delta = if self.reverse {
            self.anchor_pos - pos
        } else {
            pos - self.anchor_pos
        };
        self.anchor_at + Duration::from_secs_f64(delta.max(0.0) / self.speed / 1000.0)
    }
}

struct Playing {
    rec_id: i64,
    camera: &'static CameraDef,
    end_ms: f64,
    clock: Clock,
    ended: bool,
    /// Next video frame number (recording time / frame duration).
    next_v: i64,
    /// Next audio frame number.
    next_a: i64,
}

impl Playing {
    fn new(
        rec_id: i64,
        camera: &'static CameraDef,
        end_ms: f64,
        speed: f64,
        reverse: bool,
        paused: bool,
    ) -> Playing {
        Playing {
            rec_id,
            camera,
            end_ms,
            clock: Clock {
                anchor_pos: 0.0,
                anchor_at: Instant::now(),
                speed,
                reverse,
                paused,
            },
            ended: false,
            next_v: 0,
            next_a: 0,
        }
    }

    /// Jump to `pos_ms`, snapped down to a keyframe so decoding can
    /// start there.
    fn seek_to(&mut self, pos_ms: f64, media: &Media) {
        let gop = media.gop as i64;
        let k = (pos_ms.max(0.0) / VIDEO_FRAME_MS).floor() as i64;
        let k = k - k.rem_euclid(gop);
        let pos = k as f64 * VIDEO_FRAME_MS;
        self.clock.anchor(pos, Instant::now());
        self.next_v = k;
        self.next_a = (pos / media.audio_frame_ms()).ceil() as i64;
        self.ended = false;
    }

    fn set_speed(&mut self, speed: f64) {
        let now = Instant::now();
        let pos = self.clock.pos_at(now);
        self.clock.anchor(pos, now);
        self.clock.speed = speed;
    }

    fn set_paused(&mut self, paused: bool) {
        let now = Instant::now();
        let pos = self.clock.pos_at(now);
        self.clock.anchor(pos, now);
        self.clock.paused = paused;
    }

    fn set_reverse(&mut self, reverse: bool, media: &Media) {
        if self.clock.reverse == reverse {
            return;
        }
        let pos = self.clock.pos_at(Instant::now());
        self.clock.reverse = reverse;
        self.seek_to(pos, media);
    }
}

struct Live {
    camera: &'static CameraDef,
    audio: bool,
    start: Instant,
    next_v: i64,
    next_a: i64,
}

enum Mode {
    Live(Live),
    History(Option<Playing>),
    /// Nothing to stream (disabled camera): a close notice, then silence.
    Dead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Video,
    Audio,
    Close,
}

/// One in-band text command, `k=v&k=v`.
struct Command(Vec<(String, String)>);

impl Command {
    fn parse(text: &str) -> Command {
        Command(
            text.split('&')
                .filter(|p| !p.is_empty())
                .map(|p| match p.split_once('=') {
                    Some((k, v)) => (k.to_owned(), v.to_owned()),
                    None => (p.to_owned(), String::new()),
                })
                .collect(),
        )
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn has(&self, key: &str) -> bool {
        self.0.iter().any(|(k, _)| k == key)
    }

    fn int(&self, key: &str) -> Option<i64> {
        self.get(key)?.trim().parse().ok()
    }

    fn float(&self, key: &str) -> Option<f64> {
        self.get(key)?.trim().parse().ok()
    }

    fn boolean(&self, key: &str) -> Option<bool> {
        Some(matches!(
            self.get(key)?.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes"
        ))
    }
}

struct Streamer {
    app: Arc<App>,
    media: Arc<Media>,
    conn: u64,
    tx: SplitSink<WebSocket, Message>,
    mode: Mode,
    sent_video: u64,
    sent_audio: u64,
    last_client_msg: Instant,
}

impl Streamer {
    async fn run(&mut self, mut rx: SplitStream<WebSocket>) -> String {
        if let Err(e) = self.start().await {
            return e;
        }
        let mut silence = tokio::time::interval(Duration::from_secs(5));
        silence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let (deadline, item) = match self.next_item() {
                Some((at, item)) => (at, Some(item)),
                None => (Instant::now() + Duration::from_secs(3600), None),
            };
            tokio::select! {
                msg = rx.next() => match msg {
                    Some(Ok(Message::Text(text))) => {
                        self.last_client_msg = Instant::now();
                        if let Err(e) = self.on_text(text.as_str()).await {
                            return e;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return "closed by client".into(),
                    Some(Ok(_)) => self.last_client_msg = Instant::now(),
                    Some(Err(e)) => return format!("socket error: {e}"),
                },
                _ = sleep_until(deadline), if item.is_some() => {
                    if let Some(item) = item
                        && let Err(e) = self.emit(item).await
                    {
                        return e;
                    }
                }
                _ = silence.tick() => {
                    if self.last_client_msg.elapsed() > CLIENT_SILENCE_TIMEOUT {
                        return format!("client silent for {}s", CLIENT_SILENCE_TIMEOUT.as_secs());
                    }
                }
            }
        }
    }

    async fn start(&mut self) -> Result<(), String> {
        match &self.mode {
            Mode::Live(live) => {
                let (camera, audio) = (live.camera, live.audio);
                self.send_codec_info(camera, audio).await
            }
            Mode::Dead => {
                self.send("close=1", &[]).await?;
                Err("camera not streaming".into())
            }
            Mode::History(_) => Ok(()),
        }
    }

    async fn send(&mut self, header: &str, payload: &[u8]) -> Result<(), String> {
        self.tx
            .send(Message::Binary(encode_frame(header, payload)))
            .await
            .map_err(|e| format!("send failed: {e}"))
    }

    /// The codec-info frame DSM sends first on every (re)connection:
    /// `vdoCodec` (+ `adoCodec`/`adoExtra`), payload SPS/PPS and the
    /// `<channels>|<rate>|<AudioSpecificConfig>` trailer.
    async fn send_codec_info(&mut self, camera: &CameraDef, audio: bool) -> Result<(), String> {
        let mut payload = self.media.parameter_sets.to_vec();
        let mut header = format!("vdoCodec={VIDEO_CODEC}");
        if audio {
            let mut trailer = format!(
                "{}|{}|",
                self.media.audio_channels, self.media.audio_sample_rate
            )
            .into_bytes();
            trailer.extend_from_slice(&self.media.audio_specific_config());
            header.push_str(&format!(
                "&adoCodec={AUDIO_CODEC}&adoExtra={}",
                trailer.len()
            ));
            payload.extend_from_slice(&trailer);
        }
        tracing::debug!(conn = self.conn, camera = camera.name, header, "codec info");
        self.send(&header, &payload).await
    }

    fn next_item(&self) -> Option<(Instant, Item)> {
        match &self.mode {
            Mode::Dead | Mode::History(None) => None,
            Mode::Live(live) => {
                let video_at = live.start + ms(live.next_v as f64 * VIDEO_FRAME_MS);
                if !live.audio {
                    return Some((video_at, Item::Video));
                }
                let audio_at = live.start + ms(live.next_a as f64 * self.media.audio_frame_ms());
                Some(if audio_at < video_at {
                    (audio_at, Item::Audio)
                } else {
                    (video_at, Item::Video)
                })
            }
            Mode::History(Some(p)) => {
                if p.ended || p.clock.paused {
                    return None;
                }
                let video_pos = p.next_v as f64 * VIDEO_FRAME_MS;
                let finished = if p.clock.reverse {
                    p.next_v < 0
                } else {
                    video_pos >= p.end_ms
                };
                if finished {
                    return Some((Instant::now(), Item::Close));
                }
                let video_at = p.clock.wall_for(video_pos);
                if !p.camera.audio || p.clock.reverse {
                    return Some((video_at, Item::Video));
                }
                let audio_pos = p.next_a as f64 * self.media.audio_frame_ms();
                if audio_pos >= p.end_ms {
                    return Some((video_at, Item::Video));
                }
                let audio_at = p.clock.wall_for(audio_pos);
                Some(if audio_at < video_at {
                    (audio_at, Item::Audio)
                } else {
                    (video_at, Item::Video)
                })
            }
        }
    }

    async fn emit(&mut self, item: Item) -> Result<(), String> {
        let media = Arc::clone(&self.media);
        match (&mut self.mode, item) {
            (Mode::Live(live), Item::Video) => {
                let k = live.next_v;
                live.next_v += 1;
                let t = (k as f64 * VIDEO_FRAME_MS) as u64;
                let au = media.video_frame(k);
                self.sent_video += 1;
                self.send(&format!("mediaType=1&msec={t}"), &au.payload)
                    .await
            }
            (Mode::Live(live), Item::Audio) => {
                let j = live.next_a;
                live.next_a += 1;
                let t = (j as f64 * media.audio_frame_ms()) as u64;
                let frame = media.audio_frame(j);
                self.sent_audio += 1;
                self.send(&format!("mediaType=2&msec={t}"), frame).await
            }
            (Mode::History(Some(p)), Item::Video) => {
                let k = p.next_v;
                let pos = (k as f64 * VIDEO_FRAME_MS) as u64;
                if p.clock.reverse {
                    p.next_v -= media.gop as i64;
                } else {
                    p.next_v += 1;
                }
                let au = media.video_frame(k);
                self.sent_video += 1;
                self.send(&format!("mediaType=1&msec={pos}"), &au.payload)
                    .await
            }
            (Mode::History(Some(p)), Item::Audio) => {
                let j = p.next_a;
                p.next_a += 1;
                let pos = (j as f64 * media.audio_frame_ms()) as u64;
                let frame = media.audio_frame(j);
                self.sent_audio += 1;
                self.send(&format!("mediaType=2&msec={pos}"), frame).await
            }
            (Mode::History(Some(p)), Item::Close) => {
                p.ended = true;
                let pos = if p.clock.reverse { 0 } else { p.end_ms as u64 };
                let rec_id = p.rec_id;
                tracing::info!(
                    conn = self.conn,
                    rec_id,
                    msec = pos,
                    "end of recording; close notice sent, holding"
                );
                self.send(&format!("close=1&msec={pos}"), &[]).await
            }
            _ => Ok(()),
        }
    }

    async fn on_text(&mut self, text: &str) -> Result<(), String> {
        if text == "keepAlive" {
            tracing::debug!(conn = self.conn, "keepAlive");
            return Ok(());
        }
        let cmd = Command::parse(text);
        match &self.mode {
            Mode::History(_) => self.history_command(&cmd, text).await,
            Mode::Live(_) | Mode::Dead => {
                tracing::info!(conn = self.conn, "ignoring command on live stream: {text}");
                Ok(())
            }
        }
    }

    async fn history_command(&mut self, cmd: &Command, text: &str) -> Result<(), String> {
        let conn = self.conn;
        if cmd.get("action") == Some("play") {
            return self.play(cmd).await;
        }
        let Mode::History(Some(p)) = &mut self.mode else {
            tracing::info!(conn, "command before action=play ignored: {text}");
            return Ok(());
        };
        if let Some(ms) = cmd.int("seekMs") {
            let target = (ms as f64).clamp(0.0, p.end_ms);
            p.seek_to(target, &self.media);
            tracing::info!(
                conn,
                seek_ms = ms,
                snapped_ms = p.clock.anchor_pos as u64,
                stamp = cmd.get("stamp"),
                "seek"
            );
            return Ok(());
        }
        if cmd.has("stmSrc") && cmd.int("start").is_some() {
            // The official client's `stmSrc=1&start=<s>…` re-request.
            let start_s = cmd.int("start").unwrap_or(0);
            if let Some(reverse) = cmd.boolean("reverse") {
                p.clock.reverse = reverse;
            }
            p.seek_to((start_s * 1000) as f64, &self.media);
            tracing::info!(
                conn,
                start_s,
                reverse = p.clock.reverse,
                stamp = cmd.get("stamp"),
                "native re-request"
            );
            return Ok(());
        }
        if cmd.has("pause") || cmd.has("speed") || cmd.has("reverse") {
            if let Some(speed) = cmd.float("speed").filter(|s| s.is_finite() && *s > 0.0) {
                p.set_speed(speed);
            }
            if let Some(reverse) = cmd.boolean("reverse") {
                p.set_reverse(reverse, &self.media);
            }
            if let Some(paused) = cmd.boolean("pause") {
                p.set_paused(paused);
            }
            tracing::info!(
                conn,
                speed = p.clock.speed,
                reverse = p.clock.reverse,
                paused = p.clock.paused,
                "player command: {text}"
            );
            return Ok(());
        }
        tracing::info!(conn, "unknown command ignored: {text}");
        Ok(())
    }

    async fn play(&mut self, cmd: &Command) -> Result<(), String> {
        let conn = self.conn;
        let Some(id) = cmd.int("id") else {
            tracing::warn!(conn, "action=play without id ignored");
            return Ok(());
        };
        let now = world::now_secs();
        let Some((camera, duration_s)) = self.app.world.playable(id, now) else {
            tracing::warn!(
                conn,
                id,
                "action=play for an unknown recording; close notice sent"
            );
            return self.send("close=1", &[]).await;
        };
        let duration_ms = duration_s as f64 * 1000.0;
        let end_ms = cmd
            .float("end")
            .map(|e| e * 1000.0)
            .filter(|e| *e > 0.0)
            .unwrap_or(duration_ms)
            .min(duration_ms);
        let start_ms = (cmd.float("start").unwrap_or(0.0) * 1000.0).clamp(0.0, end_ms);
        let speed = cmd
            .float("speed")
            .filter(|s| s.is_finite() && *s > 0.0)
            .unwrap_or(1.0);
        let reverse = cmd.boolean("reverse").unwrap_or(false);
        let paused = cmd.boolean("pause").unwrap_or(false);
        let restart = cmd.boolean("restart").unwrap_or(true);
        let stamp = cmd.get("stamp").unwrap_or("-").to_owned();

        let Mode::History(slot) = &mut self.mode else {
            return Ok(());
        };
        let fresh = restart || slot.as_ref().is_none_or(|p| p.rec_id != id);
        if fresh {
            let mut p = Playing::new(id, camera, end_ms, speed, reverse, paused);
            p.seek_to(start_ms, &self.media);
            *slot = Some(p);
            tracing::info!(
                conn,
                id,
                camera = camera.name,
                start_s = start_ms / 1000.0,
                end_s = end_ms / 1000.0,
                speed,
                reverse,
                paused,
                stamp,
                rec_evt_type = cmd.get("recEvtType"),
                "play"
            );
            let audio = camera.audio && cmd.boolean("blAudio").unwrap_or(true);
            self.send_codec_info(camera, audio).await
        } else {
            let p = slot.as_mut().expect("checked above");
            p.set_speed(speed);
            p.set_reverse(reverse, &self.media);
            p.set_paused(paused);
            tracing::info!(
                conn,
                id,
                speed,
                reverse,
                paused,
                stamp,
                "play update (restart=false)"
            );
            Ok(())
        }
    }
}

fn ms(v: f64) -> Duration {
    Duration::from_secs_f64(v.max(0.0) / 1000.0)
}

/// For tests and callers that want the handshake shape: the query
/// parameters a live URL carries.
pub fn parse_query(query: &str) -> HashMap<String, String> {
    let mut p = Params::default();
    p.parse_into(query.as_bytes());
    let mut map = HashMap::new();
    for key in ["method", "stmSrc", "id", "profile", "blAudio", "dsId"] {
        if let Some(v) = p.get(key) {
            map.insert(key.to_owned(), v.to_owned());
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_the_wire_format() {
        let f = encode_frame("mediaType=1&msec=42", b"xyz");
        assert_eq!(&f[..4], &[0, 0, 0, 19]);
        assert_eq!(&f[4..23], b"mediaType=1&msec=42");
        assert_eq!(&f[23..], b"xyz");
    }

    #[test]
    fn cookie_sid_is_found_among_other_cookies() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            "stay_login=1; id=abc123; x=y".parse().unwrap(),
        );
        assert_eq!(cookie_sid(&h).as_deref(), Some("abc123"));
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "idx=nope".parse().unwrap());
        assert_eq!(cookie_sid(&h), None);
    }

    #[test]
    fn commands_parse_like_the_client_builds_them() {
        let c = Command::parse(
            "action=play&method=MixStream&speed=0.5&reverse=false&start=654&end=1200&restart=true&pause=false&stamp=1&id=42",
        );
        assert_eq!(c.get("action"), Some("play"));
        assert_eq!(c.float("speed"), Some(0.5));
        assert_eq!(c.boolean("reverse"), Some(false));
        assert_eq!(c.int("id"), Some(42));
        assert!(c.has("stamp") && !c.has("seekMs"));
        let s = Command::parse("seekMs=900000&stamp=2");
        assert_eq!(s.int("seekMs"), Some(900_000));
        let q =
            parse_query("method=MixStream&stmSrc=0&blAudio=true&dsId=0&id=7&devType=1&profile=1");
        assert_eq!(q["id"], "7");
        assert_eq!(q["stmSrc"], "0");
    }

    #[test]
    fn clock_paces_at_speed_and_pauses() {
        let now = Instant::now();
        let mut c = Clock {
            anchor_pos: 1000.0,
            anchor_at: now,
            speed: 2.0,
            reverse: false,
            paused: false,
        };
        assert!((c.pos_at(now + Duration::from_secs(1)) - 3000.0).abs() < 1.0);
        assert_eq!(c.wall_for(3000.0), now + Duration::from_secs(1));
        c.paused = true;
        assert_eq!(c.pos_at(now + Duration::from_secs(5)), 1000.0);
        c.paused = false;
        c.reverse = true;
        assert!((c.pos_at(now + Duration::from_millis(250)) - 500.0).abs() < 1.0);
        assert_eq!(c.pos_at(now + Duration::from_secs(10)), 0.0);
    }
}
