//! Video sinks: feed Annex-B elementary streams to `mpv` / `ffmpeg`
//! subprocesses. No GStreamer, libmpv or GTK development packages needed;
//! only the two binaries on `PATH`.
//!
//! This is the stop-gap display/recording layer while the UI toolkit is
//! chosen. The option profiles in [`mpv::options`] are the part a later
//! native-widget (libmpv/GStreamer) crate should lift verbatim.
//!
//! # Overview
//!
//! | Type | Role | Binary |
//! |---|---|---|
//! | [`FrameSink`] | trait: `push(AccessUnit)`, `flush`, `close` | – |
//! | [`MpvPipeSink`] | display a raw H.264/H.265 pipe in an mpv window | `mpv` |
//! | [`MpvUrlPlayer`] | display an RTSP/HTTP URL or a recording file | `mpv` |
//! | [`FfmpegRemuxSink`] | record a raw pipe to `.mp4`/`.mkv` (`-c copy`) | `ffmpeg` |
//! | [`RawFileSink`] | record the elementary stream as-is | – |
//! | [`MpvIpc`] | JSON IPC: pause, seek, seek-to-live, quit, properties | – |
//!
//! `ss-stream` turns MixStream WebSocket frames into [`AccessUnit`]s
//! (payload without start code, `msec`, `vdoCodec`) and pushes them into any
//! sink; the sink prepends the Annex-B start code when missing. Nothing in
//! this crate depends on a WebSocket type.
//!
//! # How mpv gets the bytes: what was tried
//!
//! All verified against mpv 0.40.0 / FFmpeg 7.1 libs (mpv) and ffmpeg 9.0.2
//! on this machine, with a 2 s `testsrc2` H.264 and H.265 stream.
//!
//! 1. **stdin (`fd://0`) — chosen.** `tokio::process::Command` with
//!    `Stdio::piped()` gives an async `ChildStdin`; mpv is started with
//!    `--no-terminal` (mandatory: it disables mpv's own stdin/terminal
//!    handling, otherwise stdin would be treated as a key input source) and
//!    the source `fd://0`. mpv consumes the stream to EOF and exits 0 in
//!    both `-` and `fd://0` spellings. No `unsafe`, no fd bookkeeping, and
//!    the pipe is unique to the child (nothing else can inherit it).
//! 2. **Inherited descriptor (`fd://3`).** Works too (tested with a shell
//!    `3< <(…)` redirection). In Rust it needs a `pre_exec` hook calling
//!    `dup2` on a non-`CLOEXEC` descriptor, and any other child spawned
//!    concurrently would inherit the pipe unless every spawn site is
//!    careful. The reference client needs this form only because libmpv
//!    lives in-process and has no stdin of its own. Not implemented; the
//!    arguments are otherwise identical, so switching is a one-line change
//!    in `MpvProcess::spawn`.
//! 3. **Named pipe (FIFO).** Not needed: mpv opens a FIFO path like a file,
//!    but creating/unlinking it and the open-blocks-until-both-ends-open
//!    semantics buy nothing over (1).
//! 4. **`--input-ipc-server` for control** works alongside (1): the socket
//!    exists before `file-loaded`, `set_property pause`, `drop-buffers`,
//!    `seek`, `get_property` and `quit` all answer `"error":"success"` on
//!    a stdin-fed instance. mpv exits 0 on `quit`.
//!
//! Observed behaviours worth knowing:
//! * An empty stdin (EOF before any byte) makes mpv exit **2** ("no file
//!   played"); a sink that never pushed therefore reports `ProcessFailed`.
//! * With `--keep-open=yes` mpv does not exit at EOF; `MpvPipeSink::close`
//!   sends `quit` over IPC in that configuration.
//! * mpv keeps reading its input while paused, up to `demuxer-max-bytes`
//!   (512 KiB in the uncached low-latency profile), then stops draining the
//!   pipe. The producer must stop writing while paused or expect
//!   [`SinkError::WriteStalled`] (5 s by default).
//! * The pipe buffer is raised to 1 MiB (`F_SETPIPE_SZ`), as in the
//!   reference: H.265 keyframes arriving before mpv's demuxer opens would
//!   otherwise block the writer on a 64 KiB default.
//! * `drop-buffers` (our "seek to live") resets the decoder: nothing shows
//!   until the next IDR, and if the stream then ends before one arrives mpv
//!   exits 2 ("no video data played"). Fine for camera streams (IDR every
//!   1–2 s, parameter sets repeated); a test stream needs a short GOP.
//! * mpv 0.40 does not unlink its `--input-ipc-server` socket on exit;
//!   [`MpvProcess`] removes it once the child is reaped.
//! * `seek` on a pipe answers success and does nothing ("Cannot seek in
//!   this stream" in the log); `seek_to_live` is the only useful seek on a
//!   raw pipe.
//!
//! # mpv option profile (port of the reference `MpvGLArea`)
//!
//! Base: `--no-config --no-terminal --hwdec=auto-safe --keep-open=no
//! --idle=no --input-default-bindings=no --input-vo-keyboard=no --osc=no
//! --demuxer-lavf-o=rtsp_transport=tcp --tls-verify=yes --mute=no
//! --volume=100`, plus `--vo`/`--ao` when set (`SURVEILLANCE_AO`),
//! `--start=<s>` for a recording offset, `--log-file`, and finally
//! `SURVEILLANCE_MPV_OPTS` entries so they override.
//!
//! Per profile ([`PlaybackProfile`]; see [`MpvOptions::playback_options`]):
//!
//! | option | LowLatency (raw pipe, Live) | MuxedAudio (ffmpeg MKV pipe) | Default (RTSP/file) |
//! |---|---|---|---|
//! | `cache` | no (yes at history speed ≥ 2x) | yes | yes |
//! | `cache-secs` / `demuxer-readahead-secs` | 0 (+ high-speed term) | 0.5 (+) | 0.5 (+) |
//! | `demuxer-max-bytes` | 512KiB uncached / 32MiB cached | 32MiB | 32MiB |
//! | `demuxer-max-back-bytes` | 1MiB | 1MiB | 1MiB |
//! | `untimed` / `correct-pts` | yes / no (when uncached) | no / yes | no / yes |
//! | `container-fps-override` | 25 (when uncached) | 0 | 0 |
//! | `demuxer-lavf-probesize` | 32 (when uncached) | 32768 | 32768 |
//! | `demuxer-lavf-analyzeduration` | 0 | 0.3 | 0 |
//! | `demuxer-lavf-format` | h264 / hevc | auto | auto |
//!
//! # ffmpeg recording
//!
//! `ffmpeg -hide_banner -nostdin -loglevel warning [-use_wallclock_as_timestamps 1 |
//! -framerate F] -f h264|hevc -i pipe:0 -c copy [-bsf:v setts=ts=N/F/TB] -y out`.
//! ffmpeg 9 leaves raw-demuxer packets unstamped, which the Matroska muxer
//! refuses and the MP4 muxer silently drops frames on, so one of the two
//! timestamp modes is always applied (see [`ffmpeg::Timestamps`]).
//!
//! # Environment
//!
//! * `SURVEILLANCE_AO` / `SS_PLAY_AO`: audio output driver.
//! * `SURVEILLANCE_MPV_OPTS` / `SS_PLAY_MPV_OPTS`: extra mpv options,
//!   shell-split, `name=value` or bare flag.
//!
//! See docs/architecture.md and AGENTS.md for scope.

pub mod ffmpeg;
pub mod mpv;
mod process;
pub mod raw_file;
pub mod sink;

pub use ffmpeg::{FfmpegRemuxOptions, FfmpegRemuxSink, Timestamps};
pub use mpv::ipc::{MpvIpc, SeekMode};
pub use mpv::options::{CacheTuning, MpvOptions, PlaybackProfile, parse_env_opts};
pub use mpv::pipe::MpvPipeSink;
pub use mpv::process::MpvProcess;
pub use mpv::url::MpvUrlPlayer;
pub use process::{PIPE_CAPACITY, find_binary};
pub use raw_file::RawFileSink;
pub use sink::{
    ANNEXB_START_CODE, AccessUnit, Codec, FrameSink, SinkError, SinkFuture, has_start_code,
    write_annexb,
};
