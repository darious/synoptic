//! The async client: connect, parse, deliver, keep alive, detect stalls,
//! reconnect, and relay in-band commands.
//!
//! Behaviour copied from the reference's `WebSocketBridge._pump` /
//! `_read_messages` / `_send_keepalive_loop` / `_note_attempt_outcome`:
//!
//! - **Keepalive**: text `keepAlive` every 10 s ([`KEEPALIVE_INTERVAL`]).
//! - **Stall**: no message at all for 10 s (`_IDLE_TIMEOUT`) closes the
//!   socket and reconnects, except while a history stream is paused,
//!   since silence is then what was asked for.
//! - **Reconnect**: always, after a clean close, an error or a stall;
//!   delay 0.25 s doubling to 2 s, reset after any successful connection.
//!   A history reconnect re-sends `action=play` with `stamp=1` and
//!   `start=` set to where playback should have reached by now
//!   (last `msec` seen, extrapolated at the signed speed; frozen while
//!   paused). The reference notes DSM drops history sessions after about
//!   two minutes regardless, so this path is routine, not exceptional.
//! - **Give up**: five consecutive attempts that never delivered data for
//!   3 s end the task with [`Event::GaveUp`]. Closes the client asked for
//!   (cross-recording seek, live resume) are not scored.
//! - **Live pause**: frames are dropped locally; resume reconnects so the
//!   decoder restarts on a keyframe (reference `resume()`).
//! - **History pause/resume/speed/reverse/mute**: the `action=play…
//!   restart=false` update ([`HistorySession`]); seek within the
//!   recording: `seekMs=…&stamp=n`; another recording: close and
//!   reconnect with a fresh handshake.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant, MissedTickBehavior};

use crate::command::{HistorySession, KEEPALIVE_INTERVAL, KEEPALIVE_MESSAGE, PlayParams, Speed};
use crate::error::{ClientClosed, TransportError};
use crate::frame::{Frame, FrameKind, parse_frame_bytes};
use crate::transport::{Connector, Transport, TungsteniteConnector, WsMessage};

/// Reference `_IDLE_TIMEOUT`.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
/// Reference `open_timeout=15`.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Reference first retry delay.
pub const DEFAULT_INITIAL_RECONNECT_DELAY: Duration = Duration::from_millis(250);
/// Reference `_MAX_RECONNECT_DELAY`.
pub const DEFAULT_MAX_RECONNECT_DELAY: Duration = Duration::from_secs(2);
/// Reference `_FAST_FAILURE_THRESHOLD`.
pub const DEFAULT_FAST_FAILURE_THRESHOLD: Duration = Duration::from_secs(3);
/// Reference `_MAX_CONSECUTIVE_FAST_FAILURES`.
pub const DEFAULT_MAX_CONSECUTIVE_FAST_FAILURES: u32 = 5;

/// Live camera or recorded playback.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamMode {
    /// A [`crate::live_url`] connection; no handshake message.
    Live,
    /// A [`crate::history_url`] connection; `action=play` sent on connect.
    History(PlayParams),
}

/// Everything needed to run a stream.
#[derive(Clone)]
pub struct MixStreamConfig {
    /// From [`crate::live_url`] or [`crate::history_url`].
    pub url: String,
    /// The DSM session id (`_sid` from `SYNO.API.Auth`), sent as `Cookie: id=`.
    pub sid: String,
    /// Live or history.
    pub mode: StreamMode,
    /// Handshake deadline.
    pub connect_timeout: Duration,
    /// How often to send `keepAlive`.
    pub keepalive_interval: Duration,
    /// Silence that counts as a stall.
    pub idle_timeout: Duration,
    /// First reconnect delay.
    pub initial_reconnect_delay: Duration,
    /// Reconnect delay cap.
    pub max_reconnect_delay: Duration,
    /// An attempt that delivered data for at least this long resets the failure streak.
    pub fast_failure_threshold: Duration,
    /// Consecutive fast failures before giving up.
    pub max_consecutive_fast_failures: u32,
    /// Event channel depth. When full, the read loop waits, so a slow
    /// consumer backs up onto the socket (as the reference's pipe did).
    pub channel_capacity: usize,
}

impl fmt::Debug for MixStreamConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MixStreamConfig")
            .field("url", &self.url)
            .field("sid", &"<redacted>")
            .field("mode", &self.mode)
            .field("connect_timeout", &self.connect_timeout)
            .field("keepalive_interval", &self.keepalive_interval)
            .field("idle_timeout", &self.idle_timeout)
            .field("initial_reconnect_delay", &self.initial_reconnect_delay)
            .field("max_reconnect_delay", &self.max_reconnect_delay)
            .field("fast_failure_threshold", &self.fast_failure_threshold)
            .field(
                "max_consecutive_fast_failures",
                &self.max_consecutive_fast_failures,
            )
            .field("channel_capacity", &self.channel_capacity)
            .finish()
    }
}

impl MixStreamConfig {
    fn with_mode(url: impl Into<String>, sid: impl Into<String>, mode: StreamMode) -> Self {
        Self {
            url: url.into(),
            sid: sid.into(),
            mode,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            keepalive_interval: KEEPALIVE_INTERVAL,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            initial_reconnect_delay: DEFAULT_INITIAL_RECONNECT_DELAY,
            max_reconnect_delay: DEFAULT_MAX_RECONNECT_DELAY,
            fast_failure_threshold: DEFAULT_FAST_FAILURE_THRESHOLD,
            max_consecutive_fast_failures: DEFAULT_MAX_CONSECUTIVE_FAST_FAILURES,
            channel_capacity: 64,
        }
    }

    /// A live stream with the reference's timings.
    pub fn live(url: impl Into<String>, sid: impl Into<String>) -> Self {
        Self::with_mode(url, sid, StreamMode::Live)
    }

    /// A history stream with the reference's timings.
    pub fn history(url: impl Into<String>, sid: impl Into<String>, params: PlayParams) -> Self {
        Self::with_mode(url, sid, StreamMode::History(params))
    }
}

/// Why a connection ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disconnect {
    /// The server closed it (routine for DSM; the client reconnects).
    CleanClose,
    /// Nothing arrived for this long.
    Stalled(Duration),
    /// A connect or I/O error (never contains the session id).
    Error(String),
    /// The client closed it on purpose (cross-recording seek, live resume).
    Requested,
}

/// What the client reports.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A connection is up; `attempt` counts from 1 over the client's life.
    Connected {
        /// 1 for the first connection, +1 per reconnect.
        attempt: u32,
    },
    /// A parsed frame. Control frames (codec info, close notice) are
    /// delivered too; media frames with an empty payload are not.
    Frame(Frame),
    /// The connection ended; a reconnect follows unless `GaveUp` does.
    Disconnected(Disconnect),
    /// Too many consecutive failed attempts; the task is exiting.
    GaveUp(String),
    /// The task has exited (after `stop()`, `GaveUp`, or the receiver being dropped).
    Stopped,
}

#[derive(Debug)]
enum Command {
    Pause,
    Resume,
    SetSpeed(Speed),
    SetReverse(bool),
    SetMute(bool),
    SeekMs(u64),
    Play(PlayParams),
    SendRaw(String),
    Stop,
}

/// Control side of a running client. Methods are non-blocking; they fail
/// only once the task has exited.
#[derive(Debug)]
pub struct MixStreamHandle {
    cmds: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
}

impl MixStreamHandle {
    fn send(&self, cmd: Command) -> Result<(), ClientClosed> {
        self.cmds.send(cmd).map_err(|_| ClientClosed)
    }

    /// History: `pause=true` in place, position frozen. Live: drop frames locally.
    pub fn pause(&self) -> Result<(), ClientClosed> {
        self.send(Command::Pause)
    }

    /// History: `pause=false` in place. Live: reconnect for a clean keyframe.
    pub fn resume(&self) -> Result<(), ClientClosed> {
        self.send(Command::Resume)
    }

    /// History only: change speed in place.
    pub fn set_speed(&self, speed: Speed) -> Result<(), ClientClosed> {
        self.send(Command::SetSpeed(speed))
    }

    /// History only: change direction in place.
    pub fn set_reverse(&self, reverse: bool) -> Result<(), ClientClosed> {
        self.send(Command::SetReverse(reverse))
    }

    /// History only: change `mute=` in place (unverified that DSM honours it mid-stream).
    pub fn set_mute(&self, mute: bool) -> Result<(), ClientClosed> {
        self.send(Command::SetMute(mute))
    }

    /// History only: seek to `ms` into the current recording on the same
    /// connection; resumes first if paused.
    pub fn seek_ms(&self, ms: u64) -> Result<(), ClientClosed> {
        self.send(Command::SeekMs(ms))
    }

    /// History only: switch to another recording. Closes the connection
    /// and reconnects with a fresh `action=play` for `params`.
    pub fn play(&self, params: PlayParams) -> Result<(), ClientClosed> {
        self.send(Command::Play(params))
    }

    /// Send an arbitrary text message (e.g. [`crate::command::native_reseek`]).
    /// The client's stamp counter is not touched.
    pub fn send_raw(&self, text: impl Into<String>) -> Result<(), ClientClosed> {
        self.send(Command::SendRaw(text.into()))
    }

    /// Close the connection and end the task.
    pub async fn stop(self) {
        let _ = self.cmds.send(Command::Stop);
        let _ = self.task.await;
    }

    /// Abort the task without a clean close.
    pub fn abort(&self) {
        self.task.abort();
    }

    /// Whether the task is still running.
    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }
}

/// Entry points. The client is a tokio task; it talks back over the
/// returned [`Event`] receiver and takes commands via [`MixStreamHandle`].
pub struct MixStreamClient;

impl MixStreamClient {
    /// Run on the real WebSocket transport.
    pub fn connect(
        config: MixStreamConfig,
        connector: TungsteniteConnector,
    ) -> (MixStreamHandle, mpsc::Receiver<Event>) {
        Self::spawn(config, connector)
    }

    /// Run on any [`Connector`] (tests use an in-memory one).
    pub fn spawn<C: Connector>(
        config: MixStreamConfig,
        connector: C,
    ) -> (MixStreamHandle, mpsc::Receiver<Event>) {
        let (events_tx, events_rx) = mpsc::channel(config.channel_capacity.max(1));
        let (cmds_tx, cmds_rx) = mpsc::unbounded_channel();
        let runner = Runner::new(config, connector, events_tx, cmds_rx);
        let task = tokio::spawn(runner.run());
        (
            MixStreamHandle {
                cmds: cmds_tx,
                task,
            },
            events_rx,
        )
    }
}

/// Where history playback is, independent of frames arriving.
#[derive(Debug)]
struct HistoryState {
    session: HistorySession,
    /// Position (ms into the file) at `anchor_at`.
    anchor_ms: f64,
    anchor_at: Instant,
    /// Frozen position while paused.
    paused_at_ms: Option<u64>,
}

impl HistoryState {
    fn new(params: PlayParams, now: Instant) -> Self {
        let anchor_ms = (params.clamped_start_s() * 1000) as f64;
        let paused_at_ms = params.paused.then_some(anchor_ms as u64);
        Self {
            session: HistorySession::new(params),
            anchor_ms,
            anchor_at: now,
            paused_at_ms,
        }
    }

    fn end_ms(&self) -> u64 {
        self.session.params().end_s.saturating_mul(1000)
    }

    fn signed_speed(&self) -> f64 {
        let p = self.session.params();
        if p.reverse {
            -p.speed.multiplier()
        } else {
            p.speed.multiplier()
        }
    }

    /// Reference `_current_history_target`: frozen while paused, else the
    /// anchor extrapolated at the signed speed, clamped to the file.
    fn position_ms(&self, now: Instant) -> u64 {
        if let Some(p) = self.paused_at_ms {
            return p;
        }
        let elapsed = now.saturating_duration_since(self.anchor_at).as_secs_f64();
        let est = self.anchor_ms + elapsed * self.signed_speed() * 1000.0;
        (est.max(0.0) as u64).min(self.end_ms())
    }

    fn anchor(&mut self, ms: u64, now: Instant) {
        self.anchor_ms = ms.min(self.end_ms()) as f64;
        self.anchor_at = now;
    }

    /// Credit the wall clock elapsed so far to the *old* speed/direction
    /// before changing either.
    fn re_anchor(&mut self, now: Instant) {
        let p = self.position_ms(now);
        self.anchor(p, now);
    }

    /// The handshake for a fresh connection, from the current position.
    fn open_message(&mut self, now: Instant) -> String {
        let start_s = self.position_ms(now) / 1000;
        self.session.params_mut().start_s = start_s;
        self.session.open()
    }
}

enum Action {
    None,
    Send(String),
    SendMany(Vec<String>),
    Reconnect,
    Stop,
}

struct Outcome {
    delivered_data: bool,
    on_purpose: bool,
    disconnect: Disconnect,
}

struct Runner<C: Connector> {
    cfg: MixStreamConfig,
    connector: Arc<C>,
    events: mpsc::Sender<Event>,
    cmds: mpsc::UnboundedReceiver<Command>,
    history: Option<HistoryState>,
    live_paused: bool,
    fast_failures: u32,
    stopping: bool,
}

impl<C: Connector> Runner<C> {
    fn new(
        cfg: MixStreamConfig,
        connector: C,
        events: mpsc::Sender<Event>,
        cmds: mpsc::UnboundedReceiver<Command>,
    ) -> Self {
        let history = match &cfg.mode {
            StreamMode::Live => None,
            StreamMode::History(p) => Some(HistoryState::new(p.clone(), Instant::now())),
        };
        Self {
            cfg,
            connector: Arc::new(connector),
            events,
            cmds,
            history,
            live_paused: false,
            fast_failures: 0,
            stopping: false,
        }
    }

    async fn emit(&mut self, event: Event) -> bool {
        if self.events.send(event).await.is_err() {
            tracing::debug!("MixStream event receiver dropped; stopping");
            self.stopping = true;
            return false;
        }
        true
    }

    async fn run(mut self) {
        let mut delay = Duration::ZERO;
        let mut attempt = 0u32;
        while !self.stopping {
            attempt += 1;
            let started = Instant::now();
            let outcome = self.attempt(attempt).await;
            if self.stopping {
                break;
            }
            if outcome.delivered_data || outcome.on_purpose {
                // The reference resets the delay on any successful connect;
                // "delivered data" is the observable we have for that.
                delay = Duration::ZERO;
            }
            if !self
                .emit(Event::Disconnected(outcome.disconnect.clone()))
                .await
            {
                break;
            }
            if !outcome.on_purpose && self.note_attempt_outcome(outcome.delivered_data, started) {
                let reason = match outcome.disconnect {
                    Disconnect::Error(e) => e,
                    Disconnect::Stalled(d) => format!("stalled: no data for {}s", d.as_secs()),
                    Disconnect::CleanClose => "repeated connection failures".to_owned(),
                    Disconnect::Requested => "stopped".to_owned(),
                };
                tracing::error!(
                    attempts = self.fast_failures,
                    "MixStream giving up: {reason}"
                );
                self.emit(Event::GaveUp(reason)).await;
                break;
            }
            delay = if delay.is_zero() {
                self.cfg.initial_reconnect_delay
            } else {
                (delay * 2).min(self.cfg.max_reconnect_delay)
            };
            tracing::debug!(?delay, "MixStream reconnecting");
            self.sleep_handling_commands(delay).await;
        }
        let _ = self.events.try_send(Event::Stopped);
    }

    /// Reference `_note_attempt_outcome`: `true` means give up.
    fn note_attempt_outcome(&mut self, delivered_data: bool, started: Instant) -> bool {
        if delivered_data && started.elapsed() >= self.cfg.fast_failure_threshold {
            self.fast_failures = 0;
            return false;
        }
        self.fast_failures += 1;
        self.fast_failures >= self.cfg.max_consecutive_fast_failures
    }

    async fn sleep_handling_commands(&mut self, delay: Duration) {
        let sleep = time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => return,
                cmd = self.cmds.recv() => match cmd {
                    None => { self.stopping = true; return; }
                    Some(cmd) => {
                        // Offline: state changes stick and go out with the
                        // next handshake; there is nothing to send to.
                        if matches!(self.apply(cmd), Action::Stop) {
                            self.stopping = true;
                            return;
                        }
                    }
                },
            }
        }
    }

    async fn attempt(&mut self, attempt: u32) -> Outcome {
        let fail = |disconnect| Outcome {
            delivered_data: false,
            on_purpose: false,
            disconnect,
        };
        // Owned future: the command arms below need `&mut self` while it is pending.
        let connect = {
            let connector = Arc::clone(&self.connector);
            let (url, sid, timeout) = (
                self.cfg.url.clone(),
                self.cfg.sid.clone(),
                self.cfg.connect_timeout,
            );
            async move { time::timeout(timeout, connector.connect(&url, &sid)).await }
        };
        tokio::pin!(connect);
        let mut conn = loop {
            tokio::select! {
                r = &mut connect => break match r {
                    Ok(Ok(conn)) => conn,
                    Ok(Err(e)) => return fail(Disconnect::Error(e.to_string())),
                    Err(_) => return fail(Disconnect::Error(TransportError::ConnectTimeout(self.cfg.connect_timeout).to_string())),
                },
                cmd = self.cmds.recv() => match cmd {
                    None => { self.stopping = true; return fail(Disconnect::Requested); }
                    Some(cmd) => if matches!(self.apply(cmd), Action::Stop) {
                        self.stopping = true;
                        return fail(Disconnect::Requested);
                    },
                },
            }
        };
        tracing::debug!(attempt, "MixStream connected");
        if !self.emit(Event::Connected { attempt }).await {
            conn.close().await;
            return fail(Disconnect::Requested);
        }
        if let Some(h) = &mut self.history {
            let msg = h.open_message(Instant::now());
            if let Err(e) = conn.send_text(&msg).await {
                return fail(Disconnect::Error(e.to_string()));
            }
        }
        let outcome = self.pump(&mut conn).await;
        conn.close().await;
        outcome
    }

    async fn pump(&mut self, conn: &mut C::Conn) -> Outcome {
        let mut delivered_data = false;
        let done = |delivered_data, on_purpose, disconnect| Outcome {
            delivered_data,
            on_purpose,
            disconnect,
        };
        let idle = time::sleep(self.cfg.idle_timeout);
        tokio::pin!(idle);
        let mut keepalive = time::interval_at(
            Instant::now() + self.cfg.keepalive_interval,
            self.cfg.keepalive_interval,
        );
        keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);

        enum Tick {
            Msg(Result<WsMessage, TransportError>),
            Idle,
            Keepalive,
            Cmd(Option<Command>),
        }
        loop {
            let tick = tokio::select! {
                biased;
                cmd = self.cmds.recv() => Tick::Cmd(cmd),
                r = conn.recv() => Tick::Msg(r),
                _ = &mut idle => Tick::Idle,
                _ = keepalive.tick() => Tick::Keepalive,
            };
            match tick {
                Tick::Msg(Ok(WsMessage::Binary(bytes))) => {
                    delivered_data = true;
                    idle.as_mut().reset(Instant::now() + self.cfg.idle_timeout);
                    match parse_frame_bytes(bytes) {
                        Ok(frame) => {
                            if !self.deliver(frame).await {
                                return done(delivered_data, true, Disconnect::Requested);
                            }
                        }
                        Err(e) => tracing::debug!("MixStream: skipping malformed message: {e}"),
                    }
                }
                Tick::Msg(Ok(WsMessage::Text(text))) => {
                    delivered_data = true;
                    idle.as_mut().reset(Instant::now() + self.cfg.idle_timeout);
                    tracing::debug!("MixStream text message: {text}");
                }
                Tick::Msg(Ok(WsMessage::Closed(reason))) => {
                    tracing::debug!(?reason, "MixStream closed by server");
                    return done(delivered_data, false, Disconnect::CleanClose);
                }
                Tick::Msg(Err(TransportError::Closed)) => {
                    return done(delivered_data, false, Disconnect::CleanClose);
                }
                Tick::Msg(Err(e)) => {
                    return done(delivered_data, false, Disconnect::Error(e.to_string()));
                }
                Tick::Idle => {
                    let paused = self
                        .history
                        .as_ref()
                        .is_some_and(|h| h.paused_at_ms.is_some());
                    if paused {
                        // Silence is what pause=true asked for; a resumed
                        // session gets a full idle timeout from here.
                        idle.as_mut().reset(Instant::now() + self.cfg.idle_timeout);
                        continue;
                    }
                    tracing::warn!("MixStream stalled: no data for {:?}", self.cfg.idle_timeout);
                    return done(
                        delivered_data,
                        false,
                        Disconnect::Stalled(self.cfg.idle_timeout),
                    );
                }
                Tick::Keepalive => {
                    if let Err(e) = conn.send_text(KEEPALIVE_MESSAGE).await {
                        return done(delivered_data, false, Disconnect::Error(e.to_string()));
                    }
                }
                Tick::Cmd(None) => {
                    self.stopping = true;
                    return done(delivered_data, true, Disconnect::Requested);
                }
                Tick::Cmd(Some(cmd)) => {
                    let messages = match self.apply(cmd) {
                        Action::None => Vec::new(),
                        Action::Send(m) => vec![m],
                        Action::SendMany(ms) => ms,
                        Action::Reconnect => {
                            return done(delivered_data, true, Disconnect::Requested);
                        }
                        Action::Stop => {
                            self.stopping = true;
                            return done(delivered_data, true, Disconnect::Requested);
                        }
                    };
                    for m in messages {
                        if let Err(e) = conn.send_text(&m).await {
                            return done(delivered_data, false, Disconnect::Error(e.to_string()));
                        }
                    }
                }
            }
        }
    }

    /// Returns `false` once the receiver is gone.
    async fn deliver(&mut self, frame: Frame) -> bool {
        match frame.kind() {
            FrameKind::Close => {
                tracing::debug!(
                    "MixStream close notice: {}",
                    String::from_utf8_lossy(frame.header().raw())
                );
            }
            FrameKind::CodecInfo { .. } | FrameKind::Unknown => {}
            FrameKind::Video | FrameKind::Audio => {
                if frame.payload().is_empty() {
                    return true;
                }
                if self.live_paused {
                    return true;
                }
                if frame.kind() == FrameKind::Video
                    && let Some(h) = &mut self.history
                    && let Some(msec) = frame.header().msec()
                {
                    h.anchor(msec, Instant::now());
                }
            }
        }
        self.emit(Event::Frame(frame)).await
    }

    /// Apply a command to local state; the returned action is what the
    /// connection, if any, should do.
    fn apply(&mut self, cmd: Command) -> Action {
        let now = Instant::now();
        match cmd {
            Command::Stop => Action::Stop,
            Command::SendRaw(text) => Action::Send(text),
            Command::Pause => match &mut self.history {
                Some(h) => {
                    if h.paused_at_ms.is_some() {
                        return Action::None;
                    }
                    h.paused_at_ms = Some(h.position_ms(now));
                    Action::Send(h.session.pause())
                }
                None => {
                    self.live_paused = true;
                    Action::None
                }
            },
            Command::Resume => match &mut self.history {
                Some(h) => match h.paused_at_ms.take() {
                    Some(p) => {
                        h.anchor(p, now);
                        Action::Send(h.session.resume(Some(p / 1000)))
                    }
                    None => Action::None,
                },
                None => {
                    if self.live_paused {
                        self.live_paused = false;
                        Action::Reconnect
                    } else {
                        Action::None
                    }
                }
            },
            Command::SetSpeed(speed) => match &mut self.history {
                Some(h) => {
                    h.re_anchor(now);
                    Action::Send(h.session.set_speed(speed))
                }
                None => Action::None,
            },
            Command::SetReverse(reverse) => match &mut self.history {
                Some(h) => {
                    h.re_anchor(now);
                    Action::Send(h.session.set_reverse(reverse))
                }
                None => Action::None,
            },
            Command::SetMute(mute) => match &mut self.history {
                Some(h) => Action::Send(h.session.set_mute(mute)),
                None => Action::None,
            },
            Command::SeekMs(ms) => match &mut self.history {
                Some(h) => {
                    let ms = h.session.params().clamp_ms(ms);
                    let mut msgs = Vec::with_capacity(2);
                    if h.paused_at_ms.take().is_some() {
                        // A seek is "go here and play"; an explicit
                        // pause=false beats assuming a bare seekMs resumes.
                        msgs.push(h.session.resume(Some(ms / 1000)));
                    }
                    h.anchor(ms, now);
                    msgs.push(h.session.seek_ms(ms));
                    Action::SendMany(msgs)
                }
                None => Action::None,
            },
            Command::Play(params) => match &mut self.history {
                Some(h) => {
                    h.paused_at_ms = params.paused.then_some(params.clamped_start_s() * 1000);
                    h.anchor_ms = (params.clamped_start_s() * 1000) as f64;
                    h.anchor_at = now;
                    h.session.set_params(params);
                    Action::Reconnect
                }
                None => {
                    tracing::warn!(
                        "MixStream: play() ignored on a live stream; open a history URL instead"
                    );
                    Action::None
                }
            },
        }
    }
}

#[cfg(test)]
mod tests;
