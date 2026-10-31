//! In-band (text WebSocket message) commands, and the keepalive.
//!
//! Everything here is a pure function returning the exact string to send.
//! Field order and spelling follow `ws_bridge.py` `_history_play_params`,
//! `_send_history_update` and `seek()` in the reference, which were
//! captured from DSM's own Monitor Center.
//!
//! # Keepalive
//!
//! The reference sends the text message `keepAlive` every 10 s for as
//! long as a connection lives (`ws_bridge.py` `_send_keepalive_loop`,
//! `_KEEPALIVE_INTERVAL = 10.0`), with WebSocket protocol pings disabled
//! (`ping_interval=None`). `ss_webstream_task` drops a connection after
//! roughly nine missed intervals of client silence even while it keeps
//! sending video.
//!
//! # Stamps
//!
//! `stamp` is a per-connection command sequence number: the `action=play`
//! sent right after connecting carries `stamp=1`, and every later in-band
//! command increments it. A fresh (re)connection resets it to 1.
//! [`StampCounter`] and [`HistorySession`] implement that.

use std::fmt;
use std::time::Duration;

/// The keepalive text message.
pub const KEEPALIVE_MESSAGE: &str = "keepAlive";
/// How often the reference sends [`KEEPALIVE_MESSAGE`].
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// A playback speed multiplier, formatted the way DSM expects
/// (`speed=1`, `speed=0.5`, `speed=32`).
///
/// Confirmed by the reference across 0.5x to 32x: DSM really delivers
/// frames that many times faster (frame rate and `msec` both scale), so
/// no client-side pacing is needed. Untested outside that range.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Speed(f64);

impl Speed {
    /// Real time.
    pub const NORMAL: Speed = Speed(1.0);

    /// A positive, finite multiplier; `None` otherwise.
    pub fn new(multiplier: f64) -> Option<Self> {
        (multiplier.is_finite() && multiplier > 0.0).then_some(Self(multiplier))
    }

    /// The multiplier.
    pub fn multiplier(self) -> f64 {
        self.0
    }
}

impl Default for Speed {
    fn default() -> Self {
        Self::NORMAL
    }
}

impl fmt::Display for Speed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.fract() == 0.0 {
            write!(f, "{}", self.0 as u64)
        } else {
            write!(f, "{}", self.0)
        }
    }
}

/// Per-connection `stamp` sequence. The first [`StampCounter::next_stamp`] after
/// `new()` or [`StampCounter::reset`] returns 1.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StampCounter {
    last: u32,
}

impl StampCounter {
    /// A counter whose next stamp is 1.
    pub fn new() -> Self {
        Self::default()
    }

    /// The next stamp (1, 2, 3, …).
    pub fn next_stamp(&mut self) -> u32 {
        self.last += 1;
        self.last
    }

    /// The last stamp handed out, 0 if none yet.
    pub fn last(&self) -> u32 {
        self.last
    }

    /// Start over at 1, for a fresh connection.
    pub fn reset(&mut self) {
        self.last = 0;
    }
}

/// Everything the `action=play` message needs. Field names map onto the
/// reference's `Recording` model: `id` / `mount_id` / `arch_id` /
/// `event_type`, and `start`/`end` are **seconds into the file**, not
/// wall clock (`start = target_unix - rec.start_time`,
/// `end = rec.stop_time - rec.start_time`).
#[derive(Debug, Clone, PartialEq)]
pub struct PlayParams {
    /// `id=<recordingId>` (`Recording.id`, equal to `id_on_RecServer`).
    pub recording_id: u64,
    /// `mountId=` (`Recording.mount_id`).
    pub mount_id: i64,
    /// `archId=` (`Recording.arch_id`).
    pub arch_id: i64,
    /// `recEvtType=` (`Recording.event_type`).
    pub event_type: i64,
    /// `start=`: seconds into the recording to begin at.
    pub start_s: u64,
    /// `end=`: the recording's length in seconds.
    pub end_s: u64,
    /// `speed=`.
    pub speed: Speed,
    /// `reverse=`. Confirmed: `reverse=true` delivers frames with
    /// decreasing `msec`; reverse at 1x arrives markedly slower than forward.
    pub reverse: bool,
    /// `mute=`. The browser sends `true` when its player is muted; the
    /// reference found that fixing it at `true` leaves DSM sending no
    /// audio at all, so `false` is the default and muting is done locally.
    pub mute: bool,
    /// `pause=`. Confirmed both ways: `true` makes DSM stop sending
    /// entirely, `false` resumes.
    pub paused: bool,
}

impl PlayParams {
    /// Params at `start_s` into the recording, playing forward at 1x,
    /// unmuted, not paused.
    pub fn new(
        recording_id: u64,
        mount_id: i64,
        arch_id: i64,
        event_type: i64,
        start_s: u64,
        end_s: u64,
    ) -> Self {
        Self {
            recording_id,
            mount_id,
            arch_id,
            event_type,
            start_s,
            end_s,
            speed: Speed::NORMAL,
            reverse: false,
            mute: false,
            paused: false,
        }
    }

    /// `start_s` clamped to `[0, end_s]`. Past the end DSM holds on the
    /// last decoded frame instead of erroring, which looks like a freeze.
    pub fn clamped_start_s(&self) -> u64 {
        self.start_s.min(self.end_s)
    }

    /// Milliseconds clamped to the recording's length.
    pub fn clamp_ms(&self, ms: u64) -> u64 {
        ms.min(self.end_s.saturating_mul(1000))
    }
}

fn bool_str(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// The in-band `action=play` message.
///
/// `restart=true` is what the reference sends once per connection,
/// right after connecting; `restart=false` is the same message re-sent
/// to change pause/speed/reverse/mute in place without DSM jumping back
/// to `start=`. Field order is the captured one.
///
/// Confidence per field (from the reference): `start`/`end`/`stamp`/`id`/
/// `mountId`/`archId`/`recEvtType` meaning confirmed; `pause`, `speed`,
/// `reverse` confirmed by live captures; `mute` varied in captures;
/// `method`/`blAudio`/`action` by name; `blMux`/`browser`/`stmSrc`/
/// `autoDrop` copied verbatim from the browser, never varied.
pub fn history_play(p: &PlayParams, stamp: u32, restart: bool) -> String {
    format!(
        "action=play&method=MixStream&blMux=true&browser=2&stmSrc=1&blAudio=true\
         &mute={mute}&speed={speed}&reverse={reverse}&recEvtType={evt}&mountId={mount}\
         &archId={arch}&start={start}&end={end}&autoDrop=true&restart={restart}\
         &pause={pause}&stamp={stamp}&id={id}",
        mute = bool_str(p.mute),
        speed = p.speed,
        reverse = bool_str(p.reverse),
        evt = p.event_type,
        mount = p.mount_id,
        arch = p.arch_id,
        start = p.clamped_start_s(),
        end = p.end_s,
        restart = bool_str(restart),
        pause = bool_str(p.paused),
        stamp = stamp,
        id = p.recording_id,
    )
}

/// `seekMs=<ms>&stamp=<n>`: seek within the recording already selected,
/// on the same connection. A seek into a *different* recording needs a
/// new connection and a new [`history_play`].
pub fn seek_ms(ms: u64, stamp: u32) -> String {
    format!("seekMs={ms}&stamp={stamp}")
}

/// The official desktop client's history re-request, from the decompiled
/// `video_plugin.dll` (`docs/architecture.md` §5, `FUN_1800e5e00`):
///
/// - forward: `stmSrc=1&start=<s>&flushTo=<s>&reverse=false`
/// - reverse: `stmSrc=1&start=<s>&reverse=true`
///
/// with `start = positionMs / 1000`, and `&stamp=<n>` appended when
/// `stamp` is given (the string table next to the builder holds
/// `&stamp=%1`).
///
/// **Unverified on the wire**: the plugin hands this to its stream object's
/// "send in-band command" slot, but no capture of it against a NAS exists,
/// and it is not known whether DSM accepts it on a connection opened with
/// the browser-style `action=play` handshake. The browser path
/// ([`history_play`] with `restart=false`, [`seek_ms`]) is the verified one.
pub fn native_reseek(start_s: u64, reverse: bool, stamp: Option<u32>) -> String {
    let mut s = if reverse {
        format!("stmSrc=1&start={start_s}&reverse=true")
    } else {
        format!("stmSrc=1&start={start_s}&flushTo={start_s}&reverse=false")
    };
    if let Some(n) = stamp {
        s.push_str(&format!("&stamp={n}"));
    }
    s
}

/// The query-string player commands the official client's web UI sends
/// its plugin (`MainVideoLoop::SendPlayerCmd`, `docs/architecture.md`
/// §3.3/§5): `pause=true|false`, `speed=<f>`, `reverse=true|false`,
/// `seekMs=<ms>`, `volume=<int>`, `mute=true|false`.
///
/// These are **plugin-local** (UI → decoder) and, except for `seekMs`
/// (which triggers [`native_reseek`] on a WebSocket demux), are not known
/// to go over the wire. Provided for completeness; unverified as in-band
/// MixStream commands.
pub mod player_cmd {
    /// `pause=true|false`.
    pub fn pause(paused: bool) -> String {
        format!("pause={}", super::bool_str(paused))
    }
    /// `speed=<f>`. The plugin maps `f < 1` to rate `-1/f` internally.
    pub fn speed(speed: super::Speed) -> String {
        format!("speed={speed}")
    }
    /// `reverse=true|false`.
    pub fn reverse(reverse: bool) -> String {
        format!("reverse={}", super::bool_str(reverse))
    }
    /// `seekMs=<ms>`.
    pub fn seek_ms(ms: u64) -> String {
        format!("seekMs={ms}")
    }
    /// `mute=true|false`.
    pub fn mute(muted: bool) -> String {
        format!("mute={}", super::bool_str(muted))
    }
    /// `volume=<int>`.
    pub fn volume(volume: u32) -> String {
        format!("volume={volume}")
    }
}

/// State machine for one history connection's commands: owns the
/// [`PlayParams`] and the [`StampCounter`], and returns the message to
/// send for each transition. Pure: it never touches a socket.
///
/// ```
/// use ss_stream::command::{HistorySession, PlayParams, Speed};
///
/// let mut s = HistorySession::new(PlayParams::new(9001, 2, 3, 4, 654, 900));
/// let open = s.open();                       // stamp=1, restart=true
/// assert!(open.starts_with("action=play&") && open.ends_with("&stamp=1&id=9001"));
/// assert_eq!(s.seek_ms(900_000), "seekMs=900000&stamp=2");
/// assert!(s.pause().contains("&restart=false&pause=true&stamp=3&"));
/// assert!(s.set_speed(Speed::new(2.0).unwrap()).contains("&speed=2&"));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct HistorySession {
    params: PlayParams,
    stamps: StampCounter,
}

impl HistorySession {
    /// A session that has not opened a connection yet.
    pub fn new(params: PlayParams) -> Self {
        Self {
            params,
            stamps: StampCounter::new(),
        }
    }

    /// The current parameters (what the next message will carry).
    pub fn params(&self) -> &PlayParams {
        &self.params
    }

    /// Mutable access, for callers tracking position themselves.
    pub fn params_mut(&mut self) -> &mut PlayParams {
        &mut self.params
    }

    /// The stamp of the last message built, 0 before [`open`](Self::open).
    pub fn last_stamp(&self) -> u32 {
        self.stamps.last()
    }

    /// Replace the recording (a cross-recording seek). The caller must
    /// open a new connection and send [`open`](Self::open) on it.
    pub fn set_params(&mut self, params: PlayParams) {
        self.params = params;
    }

    /// The `action=play&…&restart=true&…&stamp=1` message to send right
    /// after connecting. Resets the stamp counter.
    pub fn open(&mut self) -> String {
        self.stamps.reset();
        let stamp = self.stamps.next_stamp();
        history_play(&self.params, stamp, true)
    }

    /// Re-send the current parameters with `restart=false` and the next stamp.
    pub fn update(&mut self) -> String {
        let stamp = self.stamps.next_stamp();
        history_play(&self.params, stamp, false)
    }

    /// `pause=true`, in place.
    pub fn pause(&mut self) -> String {
        self.params.paused = true;
        self.update()
    }

    /// `pause=false`, in place. `start_s`, if given, is written into the
    /// message's `start=` (the reference puts the resume position there)
    /// although with `restart=false` DSM is expected to leave it alone.
    pub fn resume(&mut self, start_s: Option<u64>) -> String {
        if let Some(s) = start_s {
            self.params.start_s = s;
        }
        self.params.paused = false;
        self.update()
    }

    /// Change speed in place.
    pub fn set_speed(&mut self, speed: Speed) -> String {
        self.params.speed = speed;
        self.update()
    }

    /// Change direction in place.
    pub fn set_reverse(&mut self, reverse: bool) -> String {
        self.params.reverse = reverse;
        self.update()
    }

    /// Change `mute=` in place. Unverified whether DSM honours a mute
    /// change mid-stream; the reference only ever sends `mute=false`.
    pub fn set_mute(&mut self, mute: bool) -> String {
        self.params.mute = mute;
        self.update()
    }

    /// `seekMs=<ms>&stamp=<n>` within the current recording, `ms` clamped
    /// to its length; `start_s` is updated so a later reconnect lands there.
    /// If the session is paused, the caller should send
    /// [`resume`](Self::resume) first (the reference does: a bare `seekMs`
    /// on a paused connection is an untested assumption).
    pub fn seek_ms(&mut self, ms: u64) -> String {
        let ms = self.params.clamp_ms(ms);
        self.params.start_s = ms / 1000;
        let stamp = self.stamps.next_stamp();
        seek_ms(ms, stamp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(msg: &str) -> Vec<(&str, &str)> {
        msg.split('&').map(|p| p.split_once('=').unwrap()).collect()
    }
    fn get<'a>(msg: &'a str, key: &str) -> &'a str {
        fields(msg)
            .into_iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
            .unwrap()
    }

    fn params() -> PlayParams {
        // rec.id=42, mount 1, arch 2, event 3, target 654 s into a 1200 s file
        PlayParams::new(42, 1, 2, 3, 654, 1200)
    }

    #[test]
    fn play_message_is_the_captured_string_in_order() {
        let msg = history_play(&params(), 1, true);
        assert_eq!(
            msg,
            "action=play&method=MixStream&blMux=true&browser=2&stmSrc=1&blAudio=true\
             &mute=false&speed=1&reverse=false&recEvtType=3&mountId=1&archId=2\
             &start=654&end=1200&autoDrop=true&restart=true&pause=false&stamp=1&id=42"
        );
        let keys: Vec<&str> = fields(&msg).into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            [
                "action",
                "method",
                "blMux",
                "browser",
                "stmSrc",
                "blAudio",
                "mute",
                "speed",
                "reverse",
                "recEvtType",
                "mountId",
                "archId",
                "start",
                "end",
                "autoDrop",
                "restart",
                "pause",
                "stamp",
                "id"
            ]
        );
    }

    #[test]
    fn play_start_is_clamped_to_end() {
        let mut p = params();
        p.start_s = 5000;
        let msg = history_play(&p, 1, true);
        assert_eq!(get(&msg, "start"), get(&msg, "end"));
    }

    #[test]
    fn speed_formats_like_dsm() {
        assert_eq!(Speed::new(1.0).unwrap().to_string(), "1");
        assert_eq!(Speed::new(0.5).unwrap().to_string(), "0.5");
        assert_eq!(Speed::new(0.25).unwrap().to_string(), "0.25");
        assert_eq!(Speed::new(32.0).unwrap().to_string(), "32");
        assert_eq!(Speed::new(0.0), None);
        assert_eq!(Speed::new(-2.0), None);
        assert_eq!(Speed::new(f64::NAN), None);
    }

    #[test]
    fn seek_and_native_strings() {
        assert_eq!(seek_ms(900_000, 2), "seekMs=900000&stamp=2");
        assert_eq!(
            native_reseek(123, false, None),
            "stmSrc=1&start=123&flushTo=123&reverse=false"
        );
        assert_eq!(
            native_reseek(123, true, None),
            "stmSrc=1&start=123&reverse=true"
        );
        assert_eq!(
            native_reseek(7, true, Some(4)),
            "stmSrc=1&start=7&reverse=true&stamp=4"
        );
        assert_eq!(player_cmd::pause(true), "pause=true");
        assert_eq!(player_cmd::speed(Speed::new(0.5).unwrap()), "speed=0.5");
        assert_eq!(player_cmd::reverse(false), "reverse=false");
        assert_eq!(player_cmd::seek_ms(10), "seekMs=10");
        assert_eq!(player_cmd::mute(true), "mute=true");
        assert_eq!(player_cmd::volume(50), "volume=50");
    }

    #[test]
    fn stamp_counter_sequences_and_resets() {
        let mut c = StampCounter::new();
        assert_eq!(c.last(), 0);
        assert_eq!((c.next_stamp(), c.next_stamp(), c.next_stamp()), (1, 2, 3));
        c.reset();
        assert_eq!(c.next_stamp(), 1);
    }

    #[test]
    fn session_open_then_seek_uses_stamps_1_and_2() {
        let mut s = HistorySession::new(params());
        let open = s.open();
        assert_eq!(get(&open, "stamp"), "1");
        assert_eq!(get(&open, "restart"), "true");
        assert_eq!(s.seek_ms(900_000), "seekMs=900000&stamp=2");
        assert_eq!(s.params().start_s, 900);
        // reopen (reconnect) resets to 1
        let reopen = s.open();
        assert_eq!(get(&reopen, "stamp"), "1");
        assert_eq!(get(&reopen, "start"), "900");
    }

    #[test]
    fn session_seek_clamps_past_the_end() {
        let mut s = HistorySession::new(params());
        s.open();
        assert_eq!(s.seek_ms(99_999_999), "seekMs=1200000&stamp=2");
    }

    #[test]
    fn session_pause_resume_speed_reverse_mute_are_in_place_updates() {
        let mut s = HistorySession::new(params());
        s.open();
        let pause = s.pause();
        assert_eq!(get(&pause, "pause"), "true");
        assert_eq!(get(&pause, "restart"), "false");
        assert_eq!(get(&pause, "stamp"), "2");

        let resume = s.resume(Some(700));
        assert_eq!(get(&resume, "pause"), "false");
        assert_eq!(get(&resume, "start"), "700");
        assert_eq!(get(&resume, "stamp"), "3");

        let speed = s.set_speed(Speed::new(8.0).unwrap());
        assert_eq!(get(&speed, "speed"), "8");
        assert_eq!(get(&speed, "restart"), "false");
        assert_eq!(get(&speed, "stamp"), "4");

        let rev = s.set_reverse(true);
        assert_eq!(get(&rev, "reverse"), "true");
        assert_eq!(get(&rev, "speed"), "8");
        assert_eq!(get(&rev, "stamp"), "5");

        let mute = s.set_mute(true);
        assert_eq!(get(&mute, "mute"), "true");
        assert_eq!(get(&mute, "stamp"), "6");
        assert_eq!(s.last_stamp(), 6);
    }

    #[test]
    fn session_reconnect_mid_pause_opens_paused() {
        let mut s = HistorySession::new(params());
        s.open();
        s.pause();
        let reopen = s.open();
        assert_eq!(get(&reopen, "pause"), "true");
        assert_eq!(get(&reopen, "stamp"), "1");
    }

    #[test]
    fn session_set_params_switches_recording() {
        let mut s = HistorySession::new(params());
        s.open();
        s.seek_ms(10_000);
        s.set_params(PlayParams::new(43, 1, 2, 3, 200, 600));
        let open = s.open();
        assert_eq!(get(&open, "id"), "43");
        assert_eq!(get(&open, "start"), "200");
        assert_eq!(get(&open, "stamp"), "1");
    }
}
