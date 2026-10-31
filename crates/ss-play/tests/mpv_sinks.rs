//! mpv as a subprocess with `--vo=null --ao=null`: no display needed.
//! Skipped (with a message) when mpv or ffmpeg/libx264 are missing.

mod common;

use std::time::Duration;

use common::{access_units, mpv, test_stream};
use ss_play::{
    Codec, FrameSink, MpvOptions, MpvPipeSink, MpvUrlPlayer, PlaybackProfile, SeekMode, SinkError,
};

fn headless(codec: Codec, dir: &std::path::Path) -> MpvOptions {
    MpvOptions::low_latency(codec)
        .headless()
        .log_file(dir.join("mpv.log"))
}

fn skip_without_mpv() -> bool {
    if mpv().is_none() {
        eprintln!("skipping: mpv not found");
        return true;
    }
    false
}

#[tokio::test]
async fn mpv_pipe_sink_consumes_the_stream_and_exits_zero() {
    if skip_without_mpv() {
        return;
    }
    for codec in [Codec::H264, Codec::H265] {
        let Some(units) = access_units(codec) else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut sink = MpvPipeSink::spawn_with(headless(codec, dir.path()), codec).unwrap();
        assert_eq!(sink.process().options().video_format, Some(codec));
        for au in units {
            sink.push(au).await.unwrap();
        }
        sink.flush().await.unwrap();
        sink.close().await.unwrap_or_else(|e| {
            panic!(
                "{codec:?}: {e}\n{}",
                std::fs::read_to_string(dir.path().join("mpv.log")).unwrap_or_default()
            )
        });
        sink.close().await.unwrap();
        let log = std::fs::read_to_string(dir.path().join("mpv.log")).unwrap_or_default();
        assert!(log.contains("EOF reached"), "mpv log:\n{log}");
        assert!(
            !sink.process().ipc_socket_path().exists(),
            "socket left behind"
        );
    }
}

#[tokio::test]
async fn mpv_pipe_sink_ipc_controls() {
    if skip_without_mpv() {
        return;
    }
    let Some(units) = access_units(Codec::H264) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut sink = MpvPipeSink::spawn_with(headless(Codec::H264, dir.path()), Codec::H264).unwrap();
    let (first, rest) = units.split_at(units.len() / 2);
    for au in first {
        sink.push(au.clone()).await.unwrap();
    }
    sink.flush().await.unwrap();

    let version = sink
        .ipc()
        .await
        .unwrap()
        .get_property("mpv-version")
        .await
        .unwrap();
    assert!(
        version.as_str().unwrap_or_default().starts_with("mpv"),
        "{version}"
    );

    sink.set_pause(true).await.unwrap();
    assert!(sink.ipc().await.unwrap().paused().await.unwrap());
    sink.set_pause(false).await.unwrap();
    assert!(!sink.ipc().await.unwrap().paused().await.unwrap());
    sink.seek_to_live().await.unwrap();
    // mpv answers success for a seek on a pipe, and nothing happens
    sink.ipc()
        .await
        .unwrap()
        .seek(100.0, SeekMode::AbsolutePercent)
        .await
        .unwrap();

    let err = sink
        .ipc()
        .await
        .unwrap()
        .get_property("no-such-property-xyz")
        .await
        .unwrap_err();
    assert!(matches!(err, SinkError::IpcCommand { .. }), "{err:?}");

    for au in rest {
        sink.push(au.clone()).await.unwrap();
    }
    // events mpv pushed while we waited for replies are kept, not lost
    let _events = sink.ipc().await.unwrap().take_events();
    sink.close().await.unwrap();
}

#[tokio::test]
async fn mpv_pipe_sink_keep_open_needs_quit() {
    if skip_without_mpv() {
        return;
    }
    let Some(units) = access_units(Codec::H264) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let opts = headless(Codec::H264, dir.path()).keep_open(true);
    let mut sink = MpvPipeSink::spawn_with(opts, Codec::H264).unwrap();
    for au in units {
        sink.push(au).await.unwrap();
    }
    sink.close_input();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        sink.process_mut().try_wait().unwrap().is_none(),
        "keep-open mpv exited at EOF"
    );
    let status = sink.quit().await.unwrap();
    assert!(status.success(), "{status}");
}

#[tokio::test]
async fn mpv_pipe_sink_rejects_wrong_codec_and_reports_closed() {
    if skip_without_mpv() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut sink = MpvPipeSink::spawn_with(headless(Codec::H264, dir.path()), Codec::H264).unwrap();
    let err = sink
        .push(ss_play::AccessUnit::new(Codec::H265, vec![0x40]))
        .await
        .unwrap_err();
    assert!(matches!(err, SinkError::CodecChanged { .. }));
    // nothing was ever written: mpv exits 2 ("no file played")
    let err = sink.close().await.unwrap_err();
    assert!(matches!(err, SinkError::ProcessFailed { .. }), "{err:?}");
    assert!(matches!(
        sink.push(ss_play::AccessUnit::new(Codec::H264, vec![0x65]))
            .await,
        Err(SinkError::Closed)
    ));
}

#[tokio::test]
async fn mpv_url_player_plays_a_local_file_and_exits_zero() {
    if skip_without_mpv() {
        return;
    }
    let Some(stream) = test_stream(Codec::H264) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("clip.h264");
    std::fs::write(&file, &stream).unwrap();
    let opts = MpvOptions::url()
        .headless()
        .log_file(dir.path().join("mpv.log"));
    assert_eq!(opts.profile, PlaybackProfile::Default);
    let mut player = MpvUrlPlayer::spawn_with(opts, &file.display().to_string()).unwrap();
    assert_eq!(player.url(), file.display().to_string());
    let paused = player.ipc().await.unwrap().paused().await.unwrap();
    assert!(!paused);
    player.wait_checked().await.unwrap_or_else(|e| {
        panic!(
            "{e}\n{}",
            std::fs::read_to_string(dir.path().join("mpv.log")).unwrap_or_default()
        )
    });
}

#[tokio::test]
async fn mpv_url_player_quit_over_ipc() {
    if skip_without_mpv() {
        return;
    }
    let Some(stream) = test_stream(Codec::H264) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("clip.h264");
    std::fs::write(&file, &stream).unwrap();
    // keep_open so mpv would otherwise sit on the last frame forever
    let opts = MpvOptions::url().headless().keep_open(true);
    let mut player = MpvUrlPlayer::spawn_with(opts, &file.display().to_string()).unwrap();
    player.set_pause(true).await.unwrap();
    let status = player.quit().await.unwrap();
    assert!(status.success(), "{status}");
}

#[tokio::test]
async fn missing_mpv_is_a_clear_error() {
    let mut opts = MpvOptions::low_latency(Codec::H264);
    opts.binary = "ss-play-no-such-mpv".into();
    let err = MpvPipeSink::spawn_with(opts, Codec::H264)
        .err()
        .expect("spawn fails");
    assert!(matches!(err, SinkError::BinaryNotFound { .. }), "{err:?}");
}
