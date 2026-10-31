//! A generated H.264 stream pushed through the same frame→sink path the
//! `live` command uses, from a fake MixStream event source into a
//! `RawFileSink`, then probed with ffprobe. Skipped (with a message) when
//! ffmpeg or libx264 is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

use bytes::Bytes;
use ss_cli::pump::{self, PumpOptions, Stop};
use ss_cli::sink;
use ss_play::{Codec, find_binary};
use ss_stream::{Event, encode_frame, parse_frame};
use tokio::sync::mpsc;

const FPS: u32 = 10;
const SECONDS: u32 = 2;
const FRAMES: u64 = (FPS * SECONDS) as u64;

/// `ffmpeg -f lavfi -i testsrc2 … -c:v libx264 -f h264 -` as bytes, shaped
/// like a camera stream (IDR every 5 frames, headers repeated, no B-frames).
fn test_stream() -> Option<Bytes> {
    let ffmpeg = find_binary("ffmpeg").or_else(|| {
        eprintln!("skipping: ffmpeg not found");
        None
    })?;
    let encoders = Command::new(&ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()?;
    if !String::from_utf8_lossy(&encoders.stdout)
        .lines()
        .any(|l| l.split_whitespace().nth(1) == Some("libx264"))
    {
        eprintln!("skipping: ffmpeg lacks libx264");
        return None;
    }
    let out = Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size=320x240:rate={FPS}"),
            "-t",
            &SECONDS.to_string(),
            "-c:v",
            "libx264",
            "-x264-params",
            "keyint=5:min-keyint=5:bframes=0:repeat-headers=1",
            "-f",
            "h264",
            "-",
        ])
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.len() > 1000, "suspiciously small test stream");
    Some(Bytes::from(out.stdout))
}

/// Split an Annex-B stream into NAL units without their start codes,
/// which is what the MixStream WebSocket delivers.
fn split_nals(stream: &Bytes) -> Vec<Bytes> {
    let b = &stream[..];
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
            let code_start = if i > 0 && b[i - 1] == 0 { i - 1 } else { i };
            starts.push((code_start, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &(_, payload_start))| {
            let end = starts.get(n + 1).map_or(b.len(), |&(c, _)| c);
            stream.slice(payload_start..end)
        })
        .collect()
}

/// `(codec_name, nb_read_frames)` of the first video stream of `path`.
fn probe(path: &Path) -> (String, u64) {
    let ffprobe = find_binary("ffprobe").expect("ffprobe present whenever ffmpeg is");
    let out = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,nb_read_frames",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.lines().next().unwrap_or_default().trim().split(',');
    let codec = parts.next().unwrap_or_default().to_string();
    let frames = parts.next().unwrap_or_default().parse().unwrap_or(0);
    (codec, frames)
}

fn frame(header: &str, payload: &[u8]) -> Event {
    Event::Frame(parse_frame(&encode_frame(header.as_bytes(), payload)).unwrap())
}

/// Play the NALs as a fake MixStream session: connect, codec info, video
/// frames with `msec`, then the server-side stop.
fn fake_source(nals: Vec<Bytes>, codec_header: Option<&'static str>) -> mpsc::Receiver<Event> {
    let (tx, rx) = mpsc::channel(8); // small on purpose: exercises back-pressure
    tokio::spawn(async move {
        tx.send(Event::Connected { attempt: 1 }).await.unwrap();
        if let Some(h) = codec_header {
            tx.send(frame(h, &[])).await.unwrap();
        }
        for (i, nal) in nals.into_iter().enumerate() {
            let header = format!("mediaType=1&msec={}", i as u64 * 100);
            tx.send(frame(&header, &nal)).await.unwrap();
        }
        // An audio frame and a reconnect ride along; neither reaches the sink.
        tx.send(frame("mediaType=2", &[0xe0; 20])).await.unwrap();
        tx.send(Event::Disconnected(ss_stream::Disconnect::CleanClose))
            .await
            .unwrap();
        tx.send(Event::Connected { attempt: 2 }).await.unwrap();
        tx.send(Event::Stopped).await.unwrap();
    });
    rx
}

fn out_file(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    dir.path().join(name)
}

#[tokio::test]
async fn generated_h264_reaches_a_raw_file_through_the_live_path() {
    let Some(stream) = test_stream() else {
        return;
    };
    let nals = split_nals(&stream);
    assert!(nals.len() >= FRAMES as usize, "only {} NALs", nals.len());
    let expected_bytes: u64 = nals.iter().map(|n| n.len() as u64 + 4).sum();
    let n_nals = nals.len() as u64;

    let dir = tempfile::tempdir().unwrap();
    let path = out_file(&dir, "live.h264");
    let mut events = fake_source(nals, Some("vdoCodec=H264&adoCodec=PCMU"));
    let opts = PumpOptions {
        stats_interval: None,
        label: "test".into(),
        ..Default::default()
    };
    let stats = pump::run(
        &mut events,
        &sink::raw_to(&path),
        &opts,
        std::future::pending(),
    )
    .await
    .unwrap();

    assert_eq!(stats.frames, n_nals);
    assert_eq!(stats.bytes, expected_bytes);
    assert_eq!(stats.codec, Some(Codec::H264));
    assert_eq!(stats.audio_frames, 1);
    assert_eq!(stats.reconnects, 1);
    assert_eq!(stats.disconnects, 1);
    assert_eq!(stats.last_msec, Some((n_nals - 1) * 100));
    assert_eq!(stats.stopped_by, Stop::StreamEnded);
    assert!(stats.gave_up.is_none());

    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len() as u64, expected_bytes);
    assert!(bytes.starts_with(&ss_play::ANNEXB_START_CODE));
    // Not byte-for-byte: x264 writes 3-byte start codes inside an access
    // unit, the sink always re-adds the 4-byte form; ffprobe is the judge.
    let (codec, frames) = probe(&path);
    assert_eq!(codec, "h264");
    assert_eq!(frames, FRAMES);
}

#[tokio::test]
async fn codec_override_and_default_apply_without_codec_info() {
    let Some(stream) = test_stream() else {
        return;
    };
    let nals = split_nals(&stream);
    let dir = tempfile::tempdir().unwrap();

    // No codec-info frame at all: H.264 is assumed (with a warning).
    let path = out_file(&dir, "assumed.h264");
    let mut events = fake_source(nals.clone(), None);
    let opts = PumpOptions {
        stats_interval: None,
        ..Default::default()
    };
    let stats = pump::run(
        &mut events,
        &sink::raw_to(&path),
        &opts,
        std::future::pending(),
    )
    .await
    .unwrap();
    assert_eq!(stats.codec, Some(Codec::H264));
    assert_eq!(probe(&path), ("h264".to_string(), FRAMES));

    // --codec wins over what DSM announces.
    let path = out_file(&dir, "forced.h265");
    let mut events = fake_source(nals, Some("vdoCodec=H264"));
    let opts = PumpOptions {
        stats_interval: None,
        codec_override: Some(Codec::H265),
        ..Default::default()
    };
    let stats = pump::run(
        &mut events,
        &sink::raw_to(&path),
        &opts,
        std::future::pending(),
    )
    .await
    .unwrap();
    assert_eq!(stats.codec, Some(Codec::H265));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), stats.bytes);
}

#[tokio::test]
async fn stop_future_and_end_of_recording_stop_the_pump() {
    let Some(stream) = test_stream() else {
        return;
    };
    let nals = split_nals(&stream);
    let dir = tempfile::tempdir().unwrap();

    // stop_at_ms: forward history playback ends at the recording's end.
    let path = out_file(&dir, "end.h264");
    let mut events = fake_source(nals.clone(), Some("vdoCodec=H264"));
    let opts = PumpOptions {
        stats_interval: None,
        stop_at_ms: Some(500),
        ..Default::default()
    };
    let stats = pump::run(
        &mut events,
        &sink::raw_to(&path),
        &opts,
        std::future::pending(),
    )
    .await
    .unwrap();
    assert_eq!(stats.stopped_by, Stop::EndReached);
    assert_eq!(stats.last_msec, Some(500));
    assert_eq!(stats.frames, 6);

    // The caller's stop future (Ctrl-C / --seconds) wins over pending events.
    let path = out_file(&dir, "stopped.h264");
    let (tx, mut events) = mpsc::channel(8);
    tx.send(frame("vdoCodec=H264", &[])).await.unwrap();
    tx.send(frame("mediaType=1&msec=0", &nals[0]))
        .await
        .unwrap();
    let opts = PumpOptions {
        stats_interval: None,
        ..Default::default()
    };
    let stats = pump::run(&mut events, &sink::raw_to(&path), &opts, async {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    })
    .await
    .unwrap();
    assert_eq!(stats.stopped_by, Stop::Requested);
    assert_eq!(stats.frames, 1);
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        nals[0].len() as u64 + 4
    );
    drop(tx);
}

#[tokio::test]
async fn gave_up_is_an_error_after_closing_the_sink() {
    let dir = tempfile::tempdir().unwrap();
    let path = out_file(&dir, "lost.h264");
    let (tx, mut events) = mpsc::channel(8);
    tx.send(frame("vdoCodec=H264", &[])).await.unwrap();
    tx.send(frame("mediaType=1", &[0x65, 1, 2, 3]))
        .await
        .unwrap();
    tx.send(Event::GaveUp("repeated connection failures".into()))
        .await
        .unwrap();
    let opts = PumpOptions {
        stats_interval: None,
        ..Default::default()
    };
    let err = pump::run(
        &mut events,
        &sink::raw_to(&path),
        &opts,
        std::future::pending(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("stream lost"), "{err}");
    // The frame received before the failure was still written and synced.
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 8);
}
