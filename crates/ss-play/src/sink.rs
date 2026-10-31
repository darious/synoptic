//! The [`FrameSink`] trait and the types every sink speaks: [`AccessUnit`],
//! [`Codec`] and [`SinkError`]. Nothing here knows about WebSockets, mpv or
//! ffmpeg; `ss-stream` (or a test) produces access units, a sink consumes
//! them.

use std::process::ExitStatus;
use std::time::Duration;

use bytes::{Buf, Bytes};
use futures_util::future::BoxFuture;
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// The 4-byte Annex-B start code a sink prepends to an access unit that
/// arrives without one (the MixStream WebSocket strips it; the reference
/// client re-adds exactly these bytes before every video payload).
pub const ANNEXB_START_CODE: [u8; 4] = [0, 0, 0, 1];

/// Video codec of an elementary stream, as DSM names it in the MixStream
/// codec-info frame (`vdoCodec=H264|H265`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    H265,
}

impl Codec {
    /// libavformat's raw demuxer name (`ffmpeg -f …`, mpv
    /// `--demuxer-lavf-format=…`) for this codec's Annex-B stream.
    pub fn ffmpeg_format(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::H265 => "hevc",
        }
    }

    /// Conventional file extension for a raw elementary stream.
    pub fn file_extension(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::H265 => "h265",
        }
    }

    /// Parse the codec name DSM sends (`H264`, `H265`); also accepts the
    /// libavformat spellings (`h264`, `hevc`, `avc`) so a value round-trips
    /// through [`Codec::ffmpeg_format`]. Anything else is `None`.
    pub fn from_name(name: &str) -> Option<Codec> {
        match name.trim().to_ascii_uppercase().as_str() {
            "H264" | "H.264" | "AVC" | "AVC1" => Some(Codec::H264),
            "H265" | "H.265" | "HEVC" | "HVC1" | "HEV1" => Some(Codec::H265),
            _ => None,
        }
    }
}

/// One video access unit (or a single NAL unit; sinks do not care which)
/// of an Annex-B elementary stream.
#[derive(Clone, Debug)]
pub struct AccessUnit {
    /// The NAL bytes, with or without a leading start code.
    pub data: Bytes,
    /// Presentation time in milliseconds if the source knows it (the
    /// MixStream `msec` header field). Purely informational for the
    /// subprocess sinks: a raw elementary stream cannot carry it.
    pub pts_ms: Option<u64>,
    /// What the bytes are. Sinks fix their format on the first access unit
    /// and reject a later change with [`SinkError::CodecChanged`].
    pub codec: Codec,
}

impl AccessUnit {
    /// An access unit without a timestamp.
    pub fn new(codec: Codec, data: impl Into<Bytes>) -> Self {
        Self {
            data: data.into(),
            pts_ms: None,
            codec,
        }
    }

    /// Attach a presentation timestamp in milliseconds.
    pub fn with_pts_ms(mut self, pts_ms: u64) -> Self {
        self.pts_ms = Some(pts_ms);
        self
    }

    /// Whether `data` already begins with a 3- or 4-byte Annex-B start code.
    pub fn has_start_code(&self) -> bool {
        has_start_code(&self.data)
    }

    /// Number of bytes a sink writes for this unit, start code included.
    pub fn wire_len(&self) -> usize {
        if self.has_start_code() {
            self.data.len()
        } else {
            self.data.len() + ANNEXB_START_CODE.len()
        }
    }
}

/// Whether `data` begins with `00 00 01` or `00 00 00 01`.
pub fn has_start_code(data: &[u8]) -> bool {
    data.starts_with(&[0, 0, 1]) || data.starts_with(&ANNEXB_START_CODE)
}

/// Write one access unit in Annex-B form to `w`, prepending the start code
/// when the unit lacks one. Uses a vectored write where the writer supports
/// it, so the payload is never copied. Returns the number of bytes written.
pub async fn write_annexb<W>(w: &mut W, au: &AccessUnit) -> std::io::Result<usize>
where
    W: AsyncWrite + Unpin,
{
    let len = au.wire_len();
    if au.has_start_code() {
        let mut buf = au.data.clone();
        w.write_all_buf(&mut buf).await?;
    } else {
        let mut buf = (&ANNEXB_START_CODE[..]).chain(au.data.clone());
        w.write_all_buf(&mut buf).await?;
    }
    Ok(len)
}

/// Everything a sink can fail with.
#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    /// `push`/`flush` after `close`, or after the sink failed.
    #[error("sink is closed")]
    Closed,
    /// The stream's codec changed after the sink fixed its input format.
    #[error("codec changed mid-stream: sink was started for {expected:?}, got {got:?}")]
    CodecChanged {
        /// Codec the sink was set up for.
        expected: Codec,
        /// Codec of the offending access unit.
        got: Codec,
    },
    /// The helper binary is not installed / not on `PATH`.
    #[error("{binary} not found (is it installed and on PATH?)")]
    BinaryNotFound {
        /// Program name as spawned.
        binary: String,
    },
    /// The helper exited (or was killed) with a failure status.
    #[error("{binary} exited with {status}{}", fmt_stderr(.stderr))]
    ProcessFailed {
        /// Program name as spawned.
        binary: String,
        /// Its exit status.
        status: ExitStatus,
        /// Tail of its stderr, if captured.
        stderr: String,
    },
    /// A pipe write did not complete in time: the reader has stopped
    /// draining (wedged decoder, stalled muxer). The sink is unusable after
    /// this; tear it down and build a fresh one.
    #[error("pipe write stalled for {0:?}: downstream reader stopped draining")]
    WriteStalled(Duration),
    /// Could not reach mpv's JSON IPC socket.
    #[error("mpv IPC: {0}")]
    Ipc(String),
    /// mpv answered an IPC command with an error string.
    #[error("mpv rejected command {command}: {error}")]
    IpcCommand {
        /// The command as sent (JSON array).
        command: String,
        /// mpv's `error` field.
        error: String,
    },
    /// Any other I/O failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Malformed JSON from the IPC socket.
    #[error("mpv IPC sent malformed JSON: {0}")]
    Json(#[from] serde_json::Error),
}

fn fmt_stderr(stderr: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}

/// Future type returned by [`FrameSink`] methods; boxed so the trait stays
/// object-safe (`Box<dyn FrameSink>` is how a UI swaps display and
/// recording back ends).
pub type SinkFuture<'a, T = ()> = BoxFuture<'a, Result<T, SinkError>>;

/// A consumer of Annex-B access units.
///
/// Calls are sequential: one `push` at a time, then `flush` whenever the
/// producer wants bytes on the wire now (end of a burst, before a pause),
/// then `close` exactly once. `close` must be idempotent and must leave the
/// sink in a state where a second `close` returns `Ok(())` and `push`
/// returns [`SinkError::Closed`].
pub trait FrameSink: Send {
    /// Deliver one access unit. Implementations prepend the Annex-B start
    /// code if `au.data` has none.
    fn push(&mut self, au: AccessUnit) -> SinkFuture<'_>;

    /// Push buffered bytes through to the consumer.
    fn flush(&mut self) -> SinkFuture<'_>;

    /// Finish: flush, end the stream, and wait for the consumer (file
    /// synced, subprocess exited). Errors describe why the consumer failed,
    /// e.g. a non-zero ffmpeg exit.
    fn close(&mut self) -> SinkFuture<'_>;
}

impl FrameSink for Box<dyn FrameSink> {
    fn push(&mut self, au: AccessUnit) -> SinkFuture<'_> {
        (**self).push(au)
    }

    fn flush(&mut self) -> SinkFuture<'_> {
        (**self).flush()
    }

    fn close(&mut self) -> SinkFuture<'_> {
        (**self).close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_names_round_trip() {
        for codec in [Codec::H264, Codec::H265] {
            assert_eq!(Codec::from_name(codec.ffmpeg_format()), Some(codec));
        }
        assert_eq!(Codec::from_name("H264"), Some(Codec::H264));
        assert_eq!(Codec::from_name("H265"), Some(Codec::H265));
        assert_eq!(Codec::from_name("MJPEG"), None);
        assert_eq!(Codec::from_name(""), None);
    }

    #[test]
    fn start_code_detection() {
        assert!(has_start_code(&[0, 0, 1, 0x67]));
        assert!(has_start_code(&[0, 0, 0, 1, 0x67]));
        assert!(!has_start_code(&[0x67, 0, 0, 1]));
        assert!(!has_start_code(&[0, 0]));
        let au = AccessUnit::new(Codec::H264, vec![0x65, 1, 2]);
        assert_eq!(au.wire_len(), 7);
        let au = AccessUnit::new(Codec::H264, vec![0, 0, 1, 0x65]);
        assert_eq!(au.wire_len(), 4);
    }

    #[tokio::test]
    async fn write_annexb_prepends_only_when_missing() {
        let mut out = Vec::new();
        let n = write_annexb(&mut out, &AccessUnit::new(Codec::H264, vec![0x65, 0xaa]))
            .await
            .unwrap();
        assert_eq!(n, 6);
        assert_eq!(out, [0, 0, 0, 1, 0x65, 0xaa]);
        out.clear();
        let n = write_annexb(&mut out, &AccessUnit::new(Codec::H265, vec![0, 0, 1, 0x40]))
            .await
            .unwrap();
        assert_eq!(n, 4);
        assert_eq!(out, [0, 0, 1, 0x40]);
    }
}
