//! Shared helpers for the subprocess tests: find binaries, generate a short
//! test stream with ffmpeg, split it into NAL units, probe outputs.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use bytes::Bytes;
use ss_play::{AccessUnit, Codec, find_binary};

/// Frames per second of the generated stream.
pub const FPS: u32 = 10;
/// Duration in seconds of the generated stream.
pub const SECONDS: u32 = 2;
/// Frames in the generated stream.
pub const FRAMES: u64 = (FPS * SECONDS) as u64;

pub fn ffmpeg() -> Option<PathBuf> {
    find_binary("ffmpeg")
}

pub fn ffprobe() -> Option<PathBuf> {
    find_binary("ffprobe")
}

pub fn mpv() -> Option<PathBuf> {
    find_binary("mpv")
}

/// Names of the encoders `ffmpeg -encoders` lists.
fn encoders() -> &'static [String] {
    static ENCODERS: OnceLock<Vec<String>> = OnceLock::new();
    ENCODERS.get_or_init(|| {
        let Some(ffmpeg) = ffmpeg() else {
            return Vec::new();
        };
        let out = Command::new(ffmpeg)
            .args(["-hide_banner", "-encoders"])
            .output()
            .ok();
        let Some(out) = out else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.split_whitespace().nth(1).map(str::to_string))
            .collect()
    })
}

pub fn encoder_for(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "libx264",
        Codec::H265 => "libx265",
    }
}

pub fn has_encoder(codec: Codec) -> bool {
    encoders().iter().any(|e| e == encoder_for(codec))
}

/// `ffmpeg -f lavfi -i testsrc2=size=320x240:rate=10 -t 2 -c:v libx264 -f h264 -`
/// as bytes, or `None` (with a printed reason) when ffmpeg or the encoder
/// is missing. Cached per codec.
pub fn test_stream(codec: Codec) -> Option<Bytes> {
    static H264: OnceLock<Option<Bytes>> = OnceLock::new();
    static H265: OnceLock<Option<Bytes>> = OnceLock::new();
    let cell = match codec {
        Codec::H264 => &H264,
        Codec::H265 => &H265,
    };
    cell.get_or_init(|| generate(codec)).clone()
}

fn generate(codec: Codec) -> Option<Bytes> {
    let Some(ffmpeg) = ffmpeg() else {
        eprintln!("skipping: ffmpeg not found");
        return None;
    };
    if !has_encoder(codec) {
        eprintln!("skipping: ffmpeg lacks the {} encoder", encoder_for(codec));
        return None;
    }
    let out = Command::new(ffmpeg)
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
            encoder_for(codec),
            // Shaped like a camera stream: an IDR every 5 frames with the
            // SPS/PPS repeated before it (so decoding can restart after
            // mpv's `drop-buffers`), and no B-frames (decode order ==
            // presentation order, so a constant-rate remux has monotonic
            // timestamps).
            "-x264-params",
            "keyint=5:min-keyint=5:bframes=0:repeat-headers=1",
            "-x265-params",
            "keyint=5:min-keyint=5:bframes=0:repeat-headers=1:log-level=none",
            "-f",
            codec.ffmpeg_format(),
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

/// Split an Annex-B stream into NAL units *without* their start codes,
/// mimicking what the MixStream WebSocket delivers.
pub fn split_nals(stream: &Bytes) -> Vec<Bytes> {
    let mut starts = Vec::new();
    let b = &stream[..];
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
    let mut nals = Vec::with_capacity(starts.len());
    for (n, &(_, payload_start)) in starts.iter().enumerate() {
        let end = starts
            .get(n + 1)
            .map_or(b.len(), |&(next_code, _)| next_code);
        nals.push(stream.slice(payload_start..end));
    }
    nals
}

/// The test stream as access units without start codes.
pub fn access_units(codec: Codec) -> Option<Vec<AccessUnit>> {
    let stream = test_stream(codec)?;
    let nals = split_nals(&stream);
    assert!(nals.len() >= FRAMES as usize, "only {} NALs", nals.len());
    Some(
        nals.into_iter()
            .enumerate()
            .map(|(i, nal)| AccessUnit::new(codec, nal).with_pts_ms(i as u64 * 100))
            .collect(),
    )
}

/// `(codec_name, nb_read_frames)` of the first video stream of `path`.
pub fn probe(path: &std::path::Path) -> (String, u64) {
    let ffprobe = ffprobe().expect("ffprobe present whenever ffmpeg is");
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
        "ffprobe failed on {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or_default();
    let mut parts = line.trim().split(',');
    let codec = parts.next().unwrap_or_default().to_string();
    let frames = parts.next().unwrap_or_default().parse().unwrap_or(0);
    (codec, frames)
}

/// Rust-side sanity check of `split_nals` and the generator.
pub fn assert_stream_sane(codec: Codec) {
    let Some(stream) = test_stream(codec) else {
        return;
    };
    assert!(stream.starts_with(&[0, 0, 0, 1]));
    let nals = split_nals(&stream);
    let total: usize = nals.iter().map(Bytes::len).sum();
    // every byte is either a start code or payload
    assert!(total < stream.len() && total + 4 * nals.len() >= stream.len());
    for nal in &nals {
        assert!(!ss_play::has_start_code(nal));
    }
}
