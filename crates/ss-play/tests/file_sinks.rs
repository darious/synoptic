//! RawFileSink and FfmpegRemuxSink against real ffmpeg/ffprobe. Skipped
//! (with a message) when ffmpeg or the encoders are missing.

mod common;

use common::{FRAMES, access_units, assert_stream_sane, ffmpeg, probe};
use ss_play::{
    Codec, FfmpegRemuxOptions, FfmpegRemuxSink, FrameSink, RawFileSink, SinkError, Timestamps,
};

#[test]
fn generated_stream_splits_into_nals() {
    assert_stream_sane(Codec::H264);
    assert_stream_sane(Codec::H265);
}

#[tokio::test]
async fn raw_file_sink_writes_a_probeable_elementary_stream() {
    for codec in [Codec::H264, Codec::H265] {
        let Some(units) = access_units(codec) else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("out.{}", codec.file_extension()));
        let mut sink = RawFileSink::create(&path).await.unwrap();
        let expected_bytes: u64 = units.iter().map(|au| au.wire_len() as u64).sum();
        for au in units {
            sink.push(au).await.unwrap();
        }
        sink.flush().await.unwrap();
        sink.close().await.unwrap();
        sink.close().await.unwrap(); // idempotent
        assert!(matches!(
            sink.push(ss_play::AccessUnit::new(codec, vec![1])).await,
            Err(SinkError::Closed)
        ));
        assert_eq!(sink.codec(), Some(codec));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), expected_bytes);
        assert_eq!(sink.bytes_written(), expected_bytes);
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(&ss_play::ANNEXB_START_CODE));
        let (name, frames) = probe(&path);
        assert_eq!(name, codec.ffmpeg_format());
        assert_eq!(frames, FRAMES, "{codec:?}");
    }
}

#[tokio::test]
async fn raw_file_sink_rejects_a_codec_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut sink = RawFileSink::create(dir.path().join("x.h264"))
        .await
        .unwrap();
    sink.push(ss_play::AccessUnit::new(Codec::H264, vec![0x67]))
        .await
        .unwrap();
    let err = sink
        .push(ss_play::AccessUnit::new(Codec::H265, vec![0x40]))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        SinkError::CodecChanged {
            expected: Codec::H264,
            got: Codec::H265
        }
    ));
}

#[tokio::test]
async fn ffmpeg_remux_constant_rate_to_mp4_and_mkv() {
    for codec in [Codec::H264, Codec::H265] {
        for ext in ["mp4", "mkv"] {
            let Some(units) = access_units(codec) else {
                return;
            };
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("out.{ext}"));
            let opts = FfmpegRemuxOptions::new(&path).timestamps(Timestamps::ConstantRate(10.0));
            let mut sink = FfmpegRemuxSink::with_options(opts);
            assert!(sink.pid().is_none(), "ffmpeg starts lazily");
            for au in units {
                sink.push(au).await.unwrap();
            }
            assert!(sink.pid().is_some());
            sink.flush().await.unwrap();
            sink.close().await.unwrap();
            sink.close().await.unwrap();
            let (name, frames) = probe(&path);
            assert_eq!(name, codec.ffmpeg_format(), "{codec:?} -> {ext}");
            assert_eq!(frames, FRAMES, "{codec:?} -> {ext}");
        }
    }
}

#[tokio::test]
async fn ffmpeg_remux_wallclock_default_produces_valid_files() {
    for ext in ["mp4", "mkv"] {
        let Some(units) = access_units(Codec::H264) else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("live.{ext}"));
        let mut sink = FfmpegRemuxSink::new(&path);
        assert_eq!(sink.options().timestamps, Timestamps::Wallclock);
        for au in units {
            sink.push(au).await.unwrap();
        }
        sink.close().await.unwrap();
        let (name, frames) = probe(&path);
        assert_eq!(name, "h264");
        assert_eq!(frames, FRAMES, "wallclock -> {ext}");
    }
}

#[tokio::test]
async fn ffmpeg_remux_reports_a_failing_ffmpeg() {
    if ffmpeg().is_none() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let Some(units) = access_units(Codec::H264) else {
        return;
    };
    // An output directory that does not exist: ffmpeg exits non-zero.
    let mut sink = FfmpegRemuxSink::new("/nonexistent-dir-ss-play/out.mp4");
    let mut result = Ok(());
    for au in units {
        result = sink.push(au).await;
        if result.is_err() {
            break;
        }
    }
    let closed = sink.close().await;
    let err = match (result, closed) {
        (Err(e), _) | (Ok(()), Err(e)) => e,
        (Ok(()), Ok(())) => panic!("expected a failure"),
    };
    match err {
        SinkError::ProcessFailed {
            binary,
            status,
            stderr,
        } => {
            assert!(binary.contains("ffmpeg"));
            assert!(!status.success());
            assert!(
                stderr.contains("nonexistent-dir-ss-play"),
                "stderr tail: {stderr:?}"
            );
        }
        SinkError::Closed | SinkError::Io(_) => {}
        other => panic!("unexpected error {other:?}"),
    }
}

#[tokio::test]
async fn missing_binary_is_a_clear_error() {
    let opts = FfmpegRemuxOptions::new("/dev/null").binary("ss-play-no-such-ffmpeg");
    let mut sink = FfmpegRemuxSink::with_options(opts);
    let err = sink.start(Codec::H264).await.unwrap_err();
    assert!(matches!(err, SinkError::BinaryNotFound { .. }), "{err:?}");
}
