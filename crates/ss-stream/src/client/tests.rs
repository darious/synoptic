//! Client behaviour on an in-memory transport, under `tokio::time::pause`.
//! Frame fixtures mirror the reference's `tests/test_ws_bridge.py`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;
use tokio::time::{self, Instant};

use super::*;
use crate::frame::{VideoCodec, encode_frame};

// ---------------------------------------------------------------- fake transport

struct FakeConn {
    incoming: mpsc::UnboundedReceiver<WsMessage>,
    sent: Arc<Mutex<Vec<String>>>,
    closed: Arc<AtomicBool>,
}

/// The test's side of one scripted connection.
#[derive(Clone)]
struct Server {
    incoming: mpsc::UnboundedSender<WsMessage>,
    sent: Arc<Mutex<Vec<String>>>,
    closed: Arc<AtomicBool>,
}

impl Server {
    fn push(&self, header: &[u8], payload: &[u8]) {
        let _ = self
            .incoming
            .send(WsMessage::Binary(Bytes::from(encode_frame(
                header, payload,
            ))));
    }
    fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

impl Transport for FakeConn {
    async fn recv(&mut self) -> Result<WsMessage, TransportError> {
        match self.incoming.recv().await {
            Some(m) => Ok(m),
            None => Ok(WsMessage::Closed(None)),
        }
    }
    async fn send_text(&mut self, text: &str) -> Result<(), TransportError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(TransportError::Closed);
        }
        self.sent.lock().unwrap().push(text.to_owned());
        Ok(())
    }
    async fn close(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

#[derive(Clone, Default)]
struct FakeConnector {
    script: Arc<Mutex<VecDeque<Result<FakeConn, TransportError>>>>,
    log: Arc<Mutex<Vec<(String, String)>>>,
}

impl FakeConnector {
    fn server(&self) -> Server {
        let (tx, rx) = mpsc::unbounded_channel();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        self.script.lock().unwrap().push_back(Ok(FakeConn {
            incoming: rx,
            sent: sent.clone(),
            closed: closed.clone(),
        }));
        Server {
            incoming: tx,
            sent,
            closed,
        }
    }
    fn failure(&self, msg: &str) {
        self.script
            .lock()
            .unwrap()
            .push_back(Err(TransportError::Connect(msg.to_owned())));
    }
    fn log(&self) -> Vec<(String, String)> {
        self.log.lock().unwrap().clone()
    }
}

impl Connector for FakeConnector {
    type Conn = FakeConn;
    async fn connect(&self, url: &str, sid: &str) -> Result<FakeConn, TransportError> {
        self.log
            .lock()
            .unwrap()
            .push((url.to_owned(), sid.to_owned()));
        let next = self.script.lock().unwrap().pop_front();
        match next {
            Some(r) => r,
            None => Err(TransportError::Connect("script exhausted".to_owned())),
        }
    }
}

// ---------------------------------------------------------------- helpers

const URL: &str = "wss://nas/ss_webstream_task/?method=MixStream&stmSrc=2&dsId=0&id=0";

fn params() -> PlayParams {
    // id 42, mount 1, arch 2, event 3; 1200 s file, start 100 s in
    PlayParams::new(42, 1, 2, 3, 100, 1200)
}

fn history_config() -> MixStreamConfig {
    MixStreamConfig::history(URL, "secret-sid", params())
}

fn live_config() -> MixStreamConfig {
    MixStreamConfig::live(
        "wss://nas/ss_webstream_task/?method=MixStream&stmSrc=0",
        "secret-sid",
    )
}

async fn next_event(rx: &mut mpsc::Receiver<Event>) -> Event {
    time::timeout(Duration::from_secs(120), rx.recv())
        .await
        .expect("timed out waiting for an event")
        .expect("event channel closed")
}

/// Let the runner process queued commands without moving the clock.
async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

fn field<'a>(msg: &'a str, key: &str) -> &'a str {
    msg.split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("no {key} in {msg}"))
}

// ---------------------------------------------------------------- tests

#[tokio::test(start_paused = true)]
async fn live_authenticates_with_sid_and_delivers_annex_b_frames() {
    let connector = FakeConnector::default();
    let server = connector.server();
    let (handle, mut rx) = MixStreamClient::spawn(live_config(), connector.clone());

    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 1 });
    let log = connector.log();
    assert_eq!(log.len(), 1);
    assert!(log[0].0.contains("stmSrc=0"));
    assert_eq!(log[0].1, "secret-sid");

    server.push(b"vdoCodec=H264", b"");
    server.push(b"mediaType=1", b""); // empty media frames are skipped
    server.push(b"mediaType=1", &[0x65, 0xAA]);
    server.push(b"mediaType=2", &[0xFF; 4]);

    let Event::Frame(codec) = next_event(&mut rx).await else {
        panic!("expected codec info")
    };
    assert!(matches!(
        codec.kind(),
        FrameKind::CodecInfo {
            video: Some(VideoCodec::H264),
            ..
        }
    ));
    let Event::Frame(video) = next_event(&mut rx).await else {
        panic!("expected video")
    };
    assert_eq!(video.kind(), FrameKind::Video);
    assert_eq!(video.annex_b(), vec![0, 0, 0, 1, 0x65, 0xAA]);
    let Event::Frame(audio) = next_event(&mut rx).await else {
        panic!("expected audio")
    };
    assert_eq!(audio.kind(), FrameKind::Audio);

    // Live sends no handshake.
    settle().await;
    assert!(server.sent().is_empty());

    handle.stop().await;
    assert_eq!(rx.recv().await, Some(Event::Stopped));
    assert!(server.closed());
}

#[tokio::test(start_paused = true)]
async fn history_handshake_then_in_band_commands_with_sequential_stamps() {
    let connector = FakeConnector::default();
    let server = connector.server();
    let (handle, mut rx) = MixStreamClient::spawn(history_config(), connector);
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 1 });
    settle().await;

    let sent = server.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0],
        "action=play&method=MixStream&blMux=true&browser=2&stmSrc=1&blAudio=true\
         &mute=false&speed=1&reverse=false&recEvtType=3&mountId=1&archId=2\
         &start=100&end=1200&autoDrop=true&restart=true&pause=false&stamp=1&id=42"
    );

    handle.seek_ms(900_000).unwrap();
    settle().await;
    assert_eq!(server.sent()[1], "seekMs=900000&stamp=2");

    handle.pause().unwrap();
    settle().await;
    let pause = &server.sent()[2];
    assert_eq!(field(pause, "pause"), "true");
    assert_eq!(field(pause, "restart"), "false");
    assert_eq!(field(pause, "stamp"), "3");
    handle.pause().unwrap(); // idempotent
    settle().await;
    assert_eq!(server.sent().len(), 3);

    handle.resume().unwrap();
    settle().await;
    let resume = &server.sent()[3];
    assert_eq!(field(resume, "pause"), "false");
    assert_eq!(field(resume, "start"), "900");
    assert_eq!(field(resume, "stamp"), "4");

    handle.set_speed(Speed::new(2.0).unwrap()).unwrap();
    handle.set_reverse(true).unwrap();
    handle.set_mute(true).unwrap();
    handle.send_raw("stmSrc=1&start=5&reverse=true").unwrap();
    settle().await;
    let sent = server.sent();
    assert_eq!(field(&sent[4], "speed"), "2");
    assert_eq!(field(&sent[4], "stamp"), "5");
    assert_eq!(field(&sent[5], "reverse"), "true");
    assert_eq!(field(&sent[5], "stamp"), "6");
    assert_eq!(field(&sent[6], "mute"), "true");
    assert_eq!(field(&sent[6], "stamp"), "7");
    assert_eq!(sent[7], "stmSrc=1&start=5&reverse=true");

    handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn seek_while_paused_resumes_first() {
    let connector = FakeConnector::default();
    let server = connector.server();
    let (handle, mut rx) = MixStreamClient::spawn(history_config(), connector);
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 1 });
    handle.pause().unwrap();
    handle.seek_ms(300_000).unwrap();
    settle().await;
    let sent = server.sent();
    assert_eq!(sent.len(), 4);
    assert_eq!(field(&sent[2], "pause"), "false");
    assert_eq!(field(&sent[2], "stamp"), "3");
    assert_eq!(sent[3], "seekMs=300000&stamp=4");
    handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn keepalive_is_sent_every_ten_seconds() {
    let connector = FakeConnector::default();
    let server = connector.server();
    let mut cfg = live_config();
    cfg.idle_timeout = Duration::from_secs(600);
    let (handle, mut rx) = MixStreamClient::spawn(cfg, connector);
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 1 });

    time::sleep(Duration::from_millis(9_900)).await;
    settle().await;
    assert_eq!(
        server.sent().iter().filter(|m| *m == "keepAlive").count(),
        0
    );
    time::sleep(Duration::from_millis(25_200)).await;
    settle().await;
    assert_eq!(
        server.sent().iter().filter(|m| *m == "keepAlive").count(),
        3
    );
    assert!(server.sent().iter().all(|m| m == "keepAlive"));
    handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn stall_reconnects_and_history_resumes_from_the_extrapolated_position() {
    let connector = FakeConnector::default();
    let first = connector.server();
    let second = connector.server();
    let (handle, mut rx) = MixStreamClient::spawn(history_config(), connector.clone());
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 1 });

    first.push(b"vdoCodec=H264", b"");
    first.push(b"mediaType=1&msec=240000", b"AAA");
    let _ = next_event(&mut rx).await;
    let Event::Frame(f) = next_event(&mut rx).await else {
        panic!()
    };
    assert_eq!(f.header().msec(), Some(240_000));
    let stalled_from = Instant::now();

    // Nothing more arrives: idle timeout fires after 10 s.
    assert_eq!(
        next_event(&mut rx).await,
        Event::Disconnected(Disconnect::Stalled(Duration::from_secs(10)))
    );
    assert!(stalled_from.elapsed() >= Duration::from_secs(10));
    assert!(first.closed());
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 2 });
    settle().await;

    let open = &second.sent()[0];
    assert_eq!(field(open, "stamp"), "1");
    assert_eq!(field(open, "id"), "42");
    // 240 s at the last frame + ~10 s stall + 0.25 s backoff at 1x -> 250 s.
    assert_eq!(field(open, "start"), "250");
    assert_eq!(connector.log().len(), 2);
    handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn reverse_playback_extrapolates_backwards_and_clamps_at_zero() {
    let connector = FakeConnector::default();
    let first = connector.server();
    let second = connector.server();
    let mut p = params();
    p.reverse = true;
    p.speed = Speed::new(4.0).unwrap();
    let (handle, mut rx) = MixStreamClient::spawn(MixStreamConfig::history(URL, "s", p), connector);
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 1 });
    first.push(b"mediaType=1&msec=60000", b"AAA");
    let _ = next_event(&mut rx).await;
    assert!(matches!(
        next_event(&mut rx).await,
        Event::Disconnected(Disconnect::Stalled(_))
    ));
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 2 });
    settle().await;
    let open = &second.sent()[0];
    // 60 s - 10.25 s * 4 = 19 s.
    assert_eq!(field(open, "start"), "19");
    assert_eq!(field(open, "reverse"), "true");
    assert_eq!(field(open, "speed"), "4");
    handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn paused_history_does_not_count_as_stalled_and_reconnects_paused() {
    let connector = FakeConnector::default();
    let first = connector.server();
    let second = connector.server();
    let (handle, mut rx) = MixStreamClient::spawn(history_config(), connector);
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 1 });
    first.push(b"mediaType=1&msec=240000", b"AAA");
    let _ = next_event(&mut rx).await;
    handle.pause().unwrap();
    settle().await;

    time::sleep(Duration::from_secs(45)).await;
    settle().await;
    assert!(rx.try_recv().is_err(), "no Disconnected while paused");
    assert!(!first.closed());
    assert!(
        server_keepalives(&first) >= 4,
        "keepalive keeps the paused session alive"
    );

    // DSM drops the session anyway: the reconnect must open paused, at the frozen position.
    drop(first);
    assert_eq!(
        next_event(&mut rx).await,
        Event::Disconnected(Disconnect::CleanClose)
    );
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 2 });
    settle().await;
    let open = &second.sent()[0];
    assert_eq!(field(open, "pause"), "true");
    assert_eq!(field(open, "start"), "240");
    assert_eq!(field(open, "stamp"), "1");

    // Still paused on the new connection: no stall there either.
    time::sleep(Duration::from_secs(30)).await;
    settle().await;
    assert!(rx.try_recv().is_err());
    handle.stop().await;
}

fn server_keepalives(s: &Server) -> usize {
    s.sent().iter().filter(|m| *m == "keepAlive").count()
}

#[tokio::test(start_paused = true)]
async fn clean_close_reconnects_with_exponential_backoff() {
    let connector = FakeConnector::default();
    let servers: Vec<Server> = (0..5).map(|_| connector.server()).collect();
    let (handle, mut rx) = MixStreamClient::spawn(live_config(), connector);
    let mut delays = Vec::new();
    for (i, s) in servers.iter().enumerate() {
        assert_eq!(
            next_event(&mut rx).await,
            Event::Connected {
                attempt: i as u32 + 1
            }
        );
        // Each connection delivers data for 4 s, so none is a "fast failure".
        s.push(b"mediaType=1", b"x");
        let _ = next_event(&mut rx).await;
        time::sleep(Duration::from_secs(4)).await;
        let _ = s.incoming.send(WsMessage::Closed(Some("1005 ".to_owned())));
        assert_eq!(
            next_event(&mut rx).await,
            Event::Disconnected(Disconnect::CleanClose)
        );
        delays.push(Instant::now());
    }
    let _ = next_event(&mut rx).await; // attempt 6 fails: script exhausted
    // Healthy connections reset the delay, so every retry waited the initial 250 ms.
    for w in delays.windows(2) {
        let gap = w[1].duration_since(w[0]);
        assert!(
            gap >= Duration::from_millis(4_250) && gap < Duration::from_millis(4_400),
            "{gap:?}"
        );
    }
    handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn gives_up_after_five_consecutive_fast_failures_with_growing_delays() {
    let connector = FakeConnector::default();
    for i in 0..5 {
        connector.failure(&format!("refused {i}"));
    }
    let (handle, mut rx) = MixStreamClient::spawn(live_config(), connector.clone());
    let t0 = Instant::now();
    let mut times = Vec::new();
    for i in 0..5 {
        assert_eq!(
            next_event(&mut rx).await,
            Event::Disconnected(Disconnect::Error(format!("connect failed: refused {i}")))
        );
        times.push(t0.elapsed());
    }
    assert_eq!(
        next_event(&mut rx).await,
        Event::GaveUp("connect failed: refused 4".to_owned())
    );
    assert_eq!(next_event(&mut rx).await, Event::Stopped);
    assert_eq!(connector.log().len(), 5);
    // 0.25, 0.5, 1, 2 s between attempts.
    let gaps: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
    assert_eq!(gaps, [250, 500, 1000, 2000].map(Duration::from_millis));
    assert!(
        !handle.is_running() || {
            handle.stop().await;
            true
        }
    );
}

#[tokio::test(start_paused = true)]
async fn play_switches_recording_without_being_scored_as_a_failure() {
    let connector = FakeConnector::default();
    let servers: Vec<Server> = (0..7).map(|_| connector.server()).collect();
    let (handle, mut rx) = MixStreamClient::spawn(history_config(), connector);
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 1 });
    for (i, s) in servers.iter().enumerate().skip(1) {
        handle
            .play(PlayParams::new(100 + i as u64, 1, 2, 3, 200, 600))
            .unwrap();
        assert_eq!(
            next_event(&mut rx).await,
            Event::Disconnected(Disconnect::Requested)
        );
        assert!(servers[i - 1].closed());
        assert_eq!(
            next_event(&mut rx).await,
            Event::Connected {
                attempt: i as u32 + 1
            }
        );
        settle().await;
        let open = &s.sent()[0];
        assert_eq!(field(open, "id"), (100 + i).to_string());
        assert_eq!(field(open, "start"), "200");
        assert_eq!(field(open, "end"), "600");
        assert_eq!(field(open, "stamp"), "1");
    }
    // Six purposeful reconnects in a row and still running.
    assert!(handle.is_running());
    handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn live_pause_drops_frames_and_resume_reconnects() {
    let connector = FakeConnector::default();
    let first = connector.server();
    let second = connector.server();
    let (handle, mut rx) = MixStreamClient::spawn(live_config(), connector);
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 1 });
    handle.pause().unwrap();
    settle().await;
    first.push(b"mediaType=1", b"dropped");
    first.push(b"vdoCodec=H264", b""); // control frames still come through
    let Event::Frame(f) = next_event(&mut rx).await else {
        panic!()
    };
    assert!(matches!(f.kind(), FrameKind::CodecInfo { .. }));
    assert!(first.sent().is_empty(), "live pause sends nothing");

    handle.resume().unwrap();
    assert_eq!(
        next_event(&mut rx).await,
        Event::Disconnected(Disconnect::Requested)
    );
    assert!(first.closed());
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 2 });
    second.push(b"mediaType=1", b"shown");
    let Event::Frame(f) = next_event(&mut rx).await else {
        panic!()
    };
    assert_eq!(f.payload().as_ref(), b"shown");
    handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn commands_while_disconnected_apply_to_the_next_handshake() {
    let connector = FakeConnector::default();
    connector.failure("down");
    let server = connector.server();
    let (handle, mut rx) = MixStreamClient::spawn(history_config(), connector);
    assert!(matches!(
        next_event(&mut rx).await,
        Event::Disconnected(Disconnect::Error(_))
    ));
    handle.set_speed(Speed::new(16.0).unwrap()).unwrap();
    handle.seek_ms(500_000).unwrap();
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 2 });
    settle().await;
    let open = &server.sent()[0];
    assert_eq!(field(open, "speed"), "16");
    assert_eq!(field(open, "stamp"), "1");
    // 500 s seek, then 0.25 s of backoff at 16x = 504 s.
    assert_eq!(field(open, "start"), "504");
    assert_eq!(server.sent().len(), 1, "offline commands are not replayed");
    handle.stop().await;
}

#[tokio::test(start_paused = true)]
async fn dropping_the_event_receiver_stops_the_task() {
    let connector = FakeConnector::default();
    let server = connector.server();
    let (handle, mut rx) = MixStreamClient::spawn(live_config(), connector);
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 1 });
    drop(rx);
    server.push(b"mediaType=1", b"x");
    time::sleep(Duration::from_secs(1)).await;
    settle().await;
    assert!(!handle.is_running());
    assert!(server.closed());
}

#[tokio::test(start_paused = true)]
async fn malformed_messages_are_skipped_not_fatal() {
    let connector = FakeConnector::default();
    let server = connector.server();
    let (handle, mut rx) = MixStreamClient::spawn(live_config(), connector);
    assert_eq!(next_event(&mut rx).await, Event::Connected { attempt: 1 });
    let _ = server.incoming.send(WsMessage::Binary(Bytes::from_static(
        b"\x00\x00\x00\x09abc",
    )));
    let _ = server.incoming.send(WsMessage::Text("hello".to_owned()));
    server.push(b"mediaType=1", b"ok");
    let Event::Frame(f) = next_event(&mut rx).await else {
        panic!()
    };
    assert_eq!(f.payload().as_ref(), b"ok");
    handle.stop().await;
}

#[test]
fn config_debug_redacts_the_sid() {
    let dbg = format!("{:?}", history_config());
    assert!(!dbg.contains("secret-sid"));
    assert!(dbg.contains("<redacted>"));
}
