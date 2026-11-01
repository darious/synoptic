//! ss-stream → GStreamer bridge: one [`VideoStream`] per playing slot.
//!
//! A tokio task (on `Services::runtime()`) runs the `MixStreamClient` and
//! forwards what matters over an `async_channel` to a GLib-side loop, which
//! pushes Annex-B access units into the slot's [`Pipeline`] and reports
//! state to the slot through a callback. Reconnect and stall handling stay
//! in ss-stream; its `Disconnected`/`GaveUp` surface here as
//! [`StreamEvent::Reconnecting`] / [`StreamEvent::Offline`].
//!
//! Seeks and resumes do not flush the pipeline: its queues hold at most
//! about a second of frames, which drain on their own, and a flush is the
//! one step that cannot be exercised on this headless machine.
//!
//! History sessions (`MixStreamConfig::history`) are driven through the
//! same object: pause/resume/speed/seek/switch-recording go straight to the
//! `MixStreamHandle`; each video frame's `msec` becomes a wall-clock
//! [`StreamEvent::Position`] for the timeline.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gtk::gdk;
use gtk::glib;
use ss_stream::{
    Disconnect, Event, FrameKind, MixStreamClient, MixStreamConfig, MixStreamHandle, PlayParams,
    Speed, TungsteniteConnector,
};

use super::audio::AudioTrack; // --- audio ---
use super::pipeline::{AudioFormat, Codec, Pipeline, PipelineEvent};

/// What the slot needs from the session to open a stream.
#[derive(Clone, Debug)]
pub struct StreamAuth {
    /// DSM base URL (`https://nas:5001`).
    pub base_url: String,
    /// Session id, sent only as the WebSocket cookie.
    pub sid: String,
    /// `verify_ssl = false` in the connection profile.
    pub insecure: bool,
}

impl StreamAuth {
    /// From the live session: the client's base URL and `_sid`, and the
    /// profile's `verify_ssl`. `None` when logged out.
    pub fn from_session(
        store: &crate::state::Store,
        services: &crate::services::Services,
    ) -> Option<StreamAuth> {
        let (base_url, sid) = services.stream_auth()?;
        let profile = store.get().connection.as_ref().map(|c| c.profile.clone());
        let insecure = profile
            .and_then(|name| store.config().profiles.get(&name).map(|p| !p.verify_ssl))
            .unwrap_or(true);
        Some(StreamAuth {
            base_url,
            sid,
            insecure,
        })
    }
}

/// The recording a history session plays (from `Recording.List`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRecording {
    pub id: u64,
    pub mount_id: i64,
    pub arch_id: i64,
    pub event_type: i64,
    /// Unix seconds.
    pub start_time: i64,
    pub stop_time: i64,
}

impl HistoryRecording {
    pub fn from_api(r: &ss_api::Recording) -> HistoryRecording {
        HistoryRecording {
            id: r.id.max(0) as u64,
            mount_id: r.mount_id,
            arch_id: r.arch_id,
            event_type: r.event_type,
            start_time: r.start_time,
            stop_time: r.stop_time,
        }
    }

    pub fn duration_s(&self) -> u64 {
        (self.stop_time - self.start_time).max(0) as u64
    }

    pub fn contains(&self, unix_secs: i64) -> bool {
        unix_secs >= self.start_time && unix_secs <= self.stop_time
    }

    /// `PlayParams` starting at wall-clock `target_secs` (clamped into the file).
    pub fn play_params(&self, target_secs: i64, speed: Speed, paused: bool) -> PlayParams {
        let start_s = (target_secs - self.start_time).clamp(0, self.duration_s() as i64) as u64;
        let mut p = PlayParams::new(
            self.id,
            self.mount_id,
            self.arch_id,
            self.event_type,
            start_s,
            self.duration_s(),
        );
        p.speed = speed;
        p.paused = paused;
        p
    }

    /// Milliseconds into the file for a wall-clock target (clamped).
    pub fn offset_ms(&self, target_secs: i64) -> u64 {
        (target_secs - self.start_time).clamp(0, self.duration_s() as i64) as u64 * 1000
    }
}

/// What a slot plays.
#[derive(Clone, Debug)]
pub enum StreamSource {
    Live {
        camera_id: i64,
        profile: ss_stream::StreamProfile,
    },
    History {
        recording: HistoryRecording,
        params: PlayParams,
    },
}

/// Reported to the slot, on the main thread.
#[derive(Clone, Debug)]
pub enum StreamEvent {
    /// Show this paintable (the pipeline was built or rebuilt for a new codec).
    Paintable(gdk::Paintable),
    /// Connected; frames are expected.
    Playing,
    /// Connection dropped; ss-stream is reconnecting.
    Reconnecting(String),
    /// ss-stream gave up or the decoder died; the caller decides on a retry.
    Offline(String),
    /// History only: wall-clock position reached, unix milliseconds.
    Position(i64),
    /// History only: the server's close notice — the recording ran out
    /// (or DSM ended the session); the slot decides what plays next.
    Ended,
}

enum Msg {
    Handle(Arc<MixStreamHandle>),
    Connected,
    Codec(ss_stream::VideoCodec),
    Video(Vec<u8>, Option<u64>),
    /// --- audio --- one decodable audio frame (ADTS AAC or raw µ-law).
    Audio(AudioFormat, Vec<u8>),
    Close,
    Disconnected(String),
    GaveUp(String),
    Stopped,
}

enum Cmd {
    Pause,
    Resume,
    Speed(Speed),
    #[allow(dead_code)] // reverse playback is not exposed in the timeline yet
    Reverse(bool),
    Mute(bool),
    SeekMs(u64),
    Play(PlayParams),
}

struct Inner {
    source: RefCell<StreamSource>,
    pipeline: RefCell<Option<Pipeline>>,
    handle: RefCell<Option<Arc<MixStreamHandle>>>,
    pending: RefCell<Vec<Cmd>>,
    on_event: Box<dyn Fn(StreamEvent)>,
    dead: Cell<bool>,
    /// --- audio --- slot's mute state, re-applied to every rebuilt pipeline.
    muted: Cell<bool>,
    /// Local gain, re-applied to every rebuilt pipeline.
    volume: Cell<f64>,
    /// `SYNOPTIC_VIDEO_DUMP=<dir>`: the first access units of this stream
    /// as an Annex-B file, to replay a camera's stream offline.
    dump: RefCell<Option<(std::fs::File, usize)>>,
}

/// Access units written per stream when dumping.
const VIDEO_DUMP_AUS: usize = 400;

fn open_video_dump(source: &StreamSource) -> Option<(std::fs::File, usize)> {
    let dir = std::env::var_os("SYNOPTIC_VIDEO_DUMP")?;
    let name = match source {
        StreamSource::Live { camera_id, .. } => format!("live-{camera_id}"),
        StreamSource::History { recording, .. } => format!("rec-{}", recording.id),
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = std::path::Path::new(&dir).join(format!("{name}-{stamp}.bin"));
    std::fs::create_dir_all(&dir).ok()?;
    let f = std::fs::File::create(&path).ok()?;
    tracing::info!("video dump: {}", path.display());
    Some((f, VIDEO_DUMP_AUS))
}

impl Inner {
    fn emit(&self, e: StreamEvent) {
        if !self.dead.get() {
            (self.on_event)(e);
        }
    }

    fn send(&self, cmd: Cmd) {
        let handle = self.handle.borrow().clone();
        match handle {
            Some(h) => {
                let r = match &cmd {
                    Cmd::Pause => h.pause(),
                    Cmd::Resume => h.resume(),
                    Cmd::Speed(s) => h.set_speed(*s),
                    Cmd::Reverse(r) => h.set_reverse(*r),
                    Cmd::Mute(m) => h.set_mute(*m),
                    Cmd::SeekMs(ms) => h.seek_ms(*ms),
                    Cmd::Play(p) => h.play(p.clone()),
                };
                if r.is_err() {
                    tracing::debug!("stream command after the client exited");
                }
            }
            None => self.pending.borrow_mut().push(cmd),
        }
    }

    fn rebuild_pipeline(self: &Rc<Self>, codec: Codec) {
        let weak = Rc::downgrade(self);
        let built = Pipeline::new(codec, move |ev| {
            if let Some(inner) = weak.upgrade() {
                match ev {
                    PipelineEvent::Error(e) => {
                        inner.pipeline.borrow_mut().take();
                        inner.emit(StreamEvent::Offline(format!("decoder: {e}")));
                    }
                    PipelineEvent::Eos => {}
                }
            }
        });
        match built {
            Ok(mut p) => {
                p.set_mute(self.muted.get()); // --- audio ---
                p.set_volume(self.volume.get());
                if let Some(paintable) = p.paintable() {
                    self.emit(StreamEvent::Paintable(paintable));
                }
                *self.pipeline.borrow_mut() = Some(p);
            }
            Err(e) => {
                tracing::warn!("building pipeline: {e}");
                self.emit(StreamEvent::Offline(format!("pipeline: {e}")));
            }
        }
    }
}

/// A running live or history stream bound to one slot. Dropping it closes
/// the WebSocket and tears the pipeline down.
pub struct VideoStream {
    inner: Rc<Inner>,
}

impl VideoStream {
    /// Open `source` and start decoding with `initial_codec` (the stream's
    /// codec-info frame may replace it).
    pub fn open(
        rt: &tokio::runtime::Handle,
        auth: &StreamAuth,
        source: StreamSource,
        initial_codec: Codec,
        on_event: impl Fn(StreamEvent) + 'static,
    ) -> Result<VideoStream, ss_stream::UrlError> {
        let cfg = match &source {
            StreamSource::Live { camera_id, profile } => {
                let url =
                    ss_stream::live_url(&auth.base_url, (*camera_id).max(0) as u32, *profile)?;
                MixStreamConfig::live(url, auth.sid.clone())
            }
            StreamSource::History { params, .. } => {
                let url = ss_stream::history_url(&auth.base_url)?;
                MixStreamConfig::history(url, auth.sid.clone(), params.clone())
            }
        };
        let connector = if auth.insecure {
            TungsteniteConnector::insecure()
        } else {
            TungsteniteConnector::new()
        };

        let inner = Rc::new(Inner {
            source: RefCell::new(source),
            pipeline: RefCell::new(None),
            handle: RefCell::new(None),
            pending: RefCell::new(Vec::new()),
            on_event: Box::new(on_event),
            dead: Cell::new(false),
            muted: Cell::new(true), // --- audio ---
            volume: Cell::new(1.0),
            dump: RefCell::new(open_video_dump(&source)),
        });
        inner.rebuild_pipeline(initial_codec);

        let (tx, rx) = async_channel::bounded::<Msg>(64);
        rt.spawn(pump(cfg, connector, tx));
        glib::spawn_future_local(receive(Rc::downgrade(&inner), rx));

        Ok(VideoStream { inner })
    }

    pub fn source(&self) -> StreamSource {
        self.inner.source.borrow().clone()
    }

    pub fn is_history(&self) -> bool {
        matches!(&*self.inner.source.borrow(), StreamSource::History { .. })
    }

    pub fn recording(&self) -> Option<HistoryRecording> {
        match &*self.inner.source.borrow() {
            StreamSource::History { recording, .. } => Some(recording.clone()),
            StreamSource::Live { .. } => None,
        }
    }

    /// History: freeze in place. Live: drop frames locally.
    pub fn pause(&self) {
        self.inner.send(Cmd::Pause);
    }

    /// History: continue. Live: reconnect for a clean keyframe.
    pub fn resume(&self) {
        self.inner.send(Cmd::Resume);
    }

    pub fn set_speed(&self, speed: Speed) {
        self.inner.send(Cmd::Speed(speed));
    }

    #[allow(dead_code)] // reverse playback is not exposed in the timeline yet
    pub fn set_reverse(&self, reverse: bool) {
        self.inner.send(Cmd::Reverse(reverse));
    }

    /// Mute/unmute local playback (the `volume` element); history
    /// sessions also get the in-band `mute=`.
    pub fn set_mute(&self, mute: bool) {
        // --- audio ---
        self.inner.muted.set(mute);
        if let Some(p) = &mut *self.inner.pipeline.borrow_mut() {
            p.set_mute(mute);
        }
        // --- end audio ---
        self.inner.send(Cmd::Mute(mute));
    }

    /// Local audio gain 0.0–1.0.
    pub fn set_volume(&self, volume: f64) {
        self.inner.volume.set(volume);
        if let Some(p) = &mut *self.inner.pipeline.borrow_mut() {
            p.set_volume(volume);
        }
    }

    /// History: seek within the current recording.
    pub fn seek_ms(&self, ms: u64) {
        self.inner.send(Cmd::SeekMs(ms));
    }

    /// History: switch to another recording on the same connection.
    pub fn play(&self, recording: HistoryRecording, params: PlayParams) {
        *self.inner.source.borrow_mut() = StreamSource::History {
            recording,
            params: params.clone(),
        };
        self.inner.send(Cmd::Play(params));
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        self.inner.dead.set(true);
        if let Some(h) = self.inner.handle.borrow_mut().take() {
            h.abort();
        }
        self.inner.pipeline.borrow_mut().take();
    }
}

/// tokio side: run the client, forward to the GLib side.
async fn pump(
    cfg: MixStreamConfig,
    connector: TungsteniteConnector,
    tx: async_channel::Sender<Msg>,
) {
    let (handle, mut events) = MixStreamClient::connect(cfg, connector);
    let handle = Arc::new(handle);
    if tx.send(Msg::Handle(handle.clone())).await.is_err() {
        handle.abort();
        return;
    }
    let mut audio = AudioTrack::new(); // --- audio ---
    while let Some(event) = events.recv().await {
        // --- audio --- codec-info and audio frames feed the track; the
        // decodable frames it yields go to the pipeline's audio appsrc.
        if let Event::Frame(frame) = &event {
            let frames = audio.feed(frame);
            if let Some(format) = audio.format() {
                for f in frames {
                    if tx.send(Msg::Audio(format, f)).await.is_err() {
                        handle.abort();
                        return;
                    }
                }
            }
        }
        // --- end audio ---
        let msg = match event {
            Event::Connected { .. } => Msg::Connected,
            Event::Frame(frame) => match frame.kind() {
                FrameKind::Video => Msg::Video(frame.annex_b(), frame.header().msec()),
                FrameKind::CodecInfo {
                    video: Some(codec), ..
                } => Msg::Codec(codec),
                FrameKind::Close => Msg::Close,
                _ => continue,
            },
            Event::Disconnected(why) => Msg::Disconnected(describe_disconnect(&why)),
            Event::GaveUp(reason) => Msg::GaveUp(reason),
            Event::Stopped => Msg::Stopped,
        };
        if tx.send(msg).await.is_err() {
            // the slot went away
            handle.abort();
            return;
        }
    }
}

/// GLib side: push frames, relay state.
async fn receive(inner: Weak<Inner>, rx: async_channel::Receiver<Msg>) {
    while let Ok(msg) = rx.recv().await {
        let Some(inner) = inner.upgrade() else { break };
        if inner.dead.get() {
            break;
        }
        match msg {
            Msg::Handle(h) => {
                *inner.handle.borrow_mut() = Some(h);
                let pending = std::mem::take(&mut *inner.pending.borrow_mut());
                for cmd in pending {
                    inner.send(cmd);
                }
            }
            Msg::Connected => inner.emit(StreamEvent::Playing),
            Msg::Codec(codec) => match Codec::from_stream(&codec) {
                Some(c) => {
                    let current = inner.pipeline.borrow().as_ref().map(|p| p.codec());
                    if current != Some(c) {
                        tracing::info!("codec {:?} → rebuilding pipeline", c);
                        inner.rebuild_pipeline(c);
                    }
                }
                None => {
                    inner.emit(StreamEvent::Offline(format!(
                        "unsupported codec {}",
                        codec.as_str()
                    )));
                }
            },
            Msg::Video(au, msec) => {
                if let Some((f, left)) = &mut *inner.dump.borrow_mut()
                    && *left > 0
                {
                    use std::io::Write;
                    *left -= 1;
                    let _ = f.write_all(&au);
                }
                if let Some(p) = &*inner.pipeline.borrow()
                    && let Err(e) = p.push(au)
                    && e != gstreamer::FlowError::Flushing
                {
                    tracing::debug!("appsrc push: {e:?}");
                }
                if let (Some(ms), StreamSource::History { recording, .. }) =
                    (msec, &*inner.source.borrow())
                {
                    inner.emit(StreamEvent::Position(
                        recording.start_time * 1000 + ms as i64,
                    ));
                }
            }
            // --- audio ---
            Msg::Audio(format, frame) => {
                if let Some(p) = &mut *inner.pipeline.borrow_mut()
                    && let Err(e) = p.push_audio(format, frame)
                    && e != gstreamer::FlowError::Flushing
                {
                    tracing::debug!("audio appsrc push: {e:?}");
                }
            }
            // --- end audio ---
            Msg::Close => {
                if matches!(&*inner.source.borrow(), StreamSource::History { .. }) {
                    inner.emit(StreamEvent::Ended);
                }
            }
            Msg::Disconnected(why) => inner.emit(StreamEvent::Reconnecting(why)),
            Msg::GaveUp(reason) => inner.emit(StreamEvent::Offline(reason)),
            Msg::Stopped => break,
        }
    }
}

fn describe_disconnect(d: &Disconnect) -> String {
    match d {
        Disconnect::CleanClose => "server closed the stream".into(),
        Disconnect::Stalled(t) => format!("no data for {} s", t.as_secs()),
        Disconnect::Error(e) => e.clone(),
        Disconnect::Requested => "reconnecting".into(),
    }
}

// --- profile resolution ----------------------------------------------------

/// The configured Live View profile for a camera: the per-camera table of
/// the connection profile, else the Settings-page dropdown.
pub fn configured_profile(
    config: &ss_config::AppConfig,
    profile_name: &str,
    camera_id: i64,
) -> ss_config::StreamProfile {
    let per_camera = config
        .profiles
        .get(profile_name)
        .and_then(|p| {
            p.state
                .camera_live_view_stream_profiles
                .get(&(camera_id.max(0) as u32))
        })
        .and_then(|s| s.parse::<ss_config::StreamProfile>().ok());
    match per_camera {
        Some(p) if p != ss_config::StreamProfile::Camera => p,
        _ => config.live_view_stream_profile(),
    }
}

/// Config profile → wire profile; `Camera` means "ask the NAS".
pub fn wire_profile(p: ss_config::StreamProfile) -> Option<ss_stream::StreamProfile> {
    match p {
        ss_config::StreamProfile::Camera => None,
        ss_config::StreamProfile::High => Some(ss_stream::StreamProfile::High),
        ss_config::StreamProfile::Balanced => Some(ss_stream::StreamProfile::Balanced),
        ss_config::StreamProfile::Low => Some(ss_stream::StreamProfile::Low),
    }
}

fn api_to_wire(p: ss_api::StreamProfile) -> ss_stream::StreamProfile {
    match p {
        ss_api::StreamProfile::High => ss_stream::StreamProfile::High,
        ss_api::StreamProfile::Balanced => ss_stream::StreamProfile::Balanced,
        ss_api::StreamProfile::Low => ss_stream::StreamProfile::Low,
    }
}

/// Resolve the profile to put in the live URL; asks the NAS
/// (`client.live_view_profile`) when the configuration says "camera".
pub async fn resolve_profile(
    client: Option<ss_api::Client>,
    camera_id: i64,
    configured: ss_config::StreamProfile,
) -> ss_stream::StreamProfile {
    if let Some(p) = wire_profile(configured) {
        return p;
    }
    match client {
        Some(c) => c
            .live_view_profile(camera_id)
            .await
            .map(api_to_wire)
            .unwrap_or_default(),
        None => ss_stream::StreamProfile::default(),
    }
}

// --- recordings for history ------------------------------------------------

/// How far around the target `find_recording` looks (seconds).
pub const RECORDING_LOOKUP_BEFORE: i64 = 12 * 3600;
pub const RECORDING_LOOKUP_AFTER: i64 = 3600;

/// The recording to play for `target_secs`: the one containing it, else the
/// first one starting after it, else the last one ending before it.
pub fn pick_recording(recs: &[HistoryRecording], target_secs: i64) -> Option<HistoryRecording> {
    if let Some(r) = recs.iter().find(|r| r.contains(target_secs)) {
        return Some(r.clone());
    }
    recs.iter()
        .filter(|r| r.start_time > target_secs)
        .min_by_key(|r| r.start_time)
        .or_else(|| {
            recs.iter()
                .filter(|r| r.stop_time < target_secs)
                .max_by_key(|r| r.stop_time)
        })
        .cloned()
}

/// The recording to continue with after `current` ends: the earliest one
/// starting at or after its end (1 s slack for back-to-back files),
/// skipping `current` itself.
pub fn pick_next(
    recs: &[HistoryRecording],
    current: &HistoryRecording,
) -> Option<HistoryRecording> {
    recs.iter()
        .filter(|r| r.id != current.id && r.start_time >= current.stop_time - 1)
        .filter(|r| r.stop_time > r.start_time)
        .min_by_key(|r| (r.start_time, r.id))
        .cloned()
}

/// `Recording.List` after `current` for one camera, then [`pick_next`].
pub async fn find_next_recording(
    client: ss_api::Client,
    camera_id: i64,
    current: HistoryRecording,
) -> Option<HistoryRecording> {
    let filter = ss_api::RecordingFilter::camera(camera_id)
        .between(
            current.stop_time - 5,
            current.stop_time + RECORDING_LOOKUP_BEFORE,
        )
        .page(0, 500);
    match client.list_recordings(&filter).await {
        Ok(page) => {
            let recs: Vec<HistoryRecording> =
                page.items.iter().map(HistoryRecording::from_api).collect();
            pick_next(&recs, &current)
        }
        Err(e) => {
            tracing::warn!("next recording for camera {camera_id}: {e}");
            None
        }
    }
}

/// `Recording.List` around `target_secs` for one camera, then [`pick_recording`].
pub async fn find_recording(
    client: ss_api::Client,
    camera_id: i64,
    target_secs: i64,
) -> Option<HistoryRecording> {
    let filter = ss_api::RecordingFilter::camera(camera_id)
        .between(
            target_secs - RECORDING_LOOKUP_BEFORE,
            target_secs + RECORDING_LOOKUP_AFTER,
        )
        .page(0, 500);
    match client.list_recordings(&filter).await {
        Ok(page) => {
            let recs: Vec<HistoryRecording> =
                page.items.iter().map(HistoryRecording::from_api).collect();
            pick_recording(&recs, target_secs)
        }
        Err(e) => {
            tracing::warn!("recordings for camera {camera_id}: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: u64, start: i64, stop: i64) -> HistoryRecording {
        HistoryRecording {
            id,
            mount_id: 0,
            arch_id: 0,
            event_type: 3,
            start_time: start,
            stop_time: stop,
        }
    }

    #[test]
    fn picks_containing_then_next_then_previous() {
        let recs = [rec(1, 100, 200), rec(2, 300, 400), rec(3, 500, 600)];
        assert_eq!(pick_recording(&recs, 350).map(|r| r.id), Some(2));
        assert_eq!(pick_recording(&recs, 250).map(|r| r.id), Some(2));
        assert_eq!(pick_recording(&recs, 450).map(|r| r.id), Some(3));
        assert_eq!(pick_recording(&recs, 700).map(|r| r.id), Some(3));
        assert_eq!(pick_recording(&recs, 50).map(|r| r.id), Some(1));
        assert_eq!(pick_recording(&[], 50), None);
    }

    #[test]
    fn next_recording_skips_the_current_and_earlier_files() {
        let recs = [
            rec(1, 100, 200),
            rec(2, 200, 300),
            rec(3, 500, 600),
            rec(4, 150, 160),
        ];
        assert_eq!(pick_next(&recs, &rec(1, 100, 200)).map(|r| r.id), Some(2));
        // a gap: the next file's start, not the end of the current one
        assert_eq!(pick_next(&recs, &rec(2, 200, 300)).map(|r| r.id), Some(3));
        assert_eq!(pick_next(&recs, &rec(3, 500, 600)), None);
        // the current file is never its own successor
        assert_eq!(pick_next(&[rec(9, 10, 20)], &rec(9, 10, 20)), None);
    }

    #[test]
    fn play_params_are_relative_to_the_file() {
        let r = rec(7, 1000, 1600);
        let p = r.play_params(1250, Speed::NORMAL, false);
        assert_eq!(p.recording_id, 7);
        assert_eq!(p.start_s, 250);
        assert_eq!(p.end_s, 600);
        // outside the file: clamped
        assert_eq!(r.play_params(10, Speed::NORMAL, false).start_s, 0);
        assert_eq!(r.play_params(5000, Speed::NORMAL, false).start_s, 600);
        assert_eq!(r.offset_ms(1250), 250_000);
        assert_eq!(r.offset_ms(1), 0);
    }

    #[test]
    fn configured_profile_prefers_the_camera_table() {
        let mut cfg = ss_config::AppConfig::default();
        let mut profile = ss_config::Profile::default();
        profile
            .state
            .camera_live_view_stream_profiles
            .insert(5, "low".into());
        profile
            .state
            .camera_live_view_stream_profiles
            .insert(6, "camera".into());
        cfg.add_profile("nas", profile);
        assert_eq!(
            configured_profile(&cfg, "nas", 5),
            ss_config::StreamProfile::Low
        );
        // "camera" and unknown cameras fall through to the settings dropdown
        assert_eq!(
            configured_profile(&cfg, "nas", 6),
            cfg.live_view_stream_profile()
        );
        assert_eq!(
            configured_profile(&cfg, "nas", 9),
            cfg.live_view_stream_profile()
        );
        assert_eq!(
            configured_profile(&cfg, "other", 5),
            cfg.live_view_stream_profile()
        );
    }

    #[test]
    fn wire_profile_maps() {
        assert_eq!(wire_profile(ss_config::StreamProfile::Camera), None);
        assert_eq!(
            wire_profile(ss_config::StreamProfile::High),
            Some(ss_stream::StreamProfile::High)
        );
        assert_eq!(
            wire_profile(ss_config::StreamProfile::Low),
            Some(ss_stream::StreamProfile::Low)
        );
    }
}
