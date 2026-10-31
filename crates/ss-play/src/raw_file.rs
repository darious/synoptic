//! [`RawFileSink`]: write the elementary stream to disk as-is (`.h264` /
//! `.h265`). ffmpeg/ffprobe/mpv all open such a file directly; it is the
//! cheapest possible recording and the right thing to keep for a bug
//! report, since it is byte-for-byte what the NAS sent plus start codes.

use std::path::{Path, PathBuf};

use tokio::fs::File;
use tokio::io::{AsyncWriteExt, BufWriter};

use crate::sink::{AccessUnit, Codec, FrameSink, SinkError, SinkFuture, write_annexb};

/// Writes Annex-B access units straight to a file.
pub struct RawFileSink {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
    codec: Option<Codec>,
    frames: u64,
    bytes: u64,
}

impl RawFileSink {
    /// Create (truncate) `path` for writing.
    pub async fn create(path: impl Into<PathBuf>) -> Result<Self, SinkError> {
        let path = path.into();
        let file = File::create(&path).await?;
        Ok(Self {
            path,
            writer: Some(BufWriter::with_capacity(256 * 1024, file)),
            codec: None,
            frames: 0,
            bytes: 0,
        })
    }

    /// Output path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Codec of the first access unit pushed, once there has been one.
    pub fn codec(&self) -> Option<Codec> {
        self.codec
    }

    /// Access units written so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Bytes written so far (start codes included).
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }
}

impl FrameSink for RawFileSink {
    fn push(&mut self, au: AccessUnit) -> SinkFuture<'_> {
        Box::pin(async move {
            let writer = self.writer.as_mut().ok_or(SinkError::Closed)?;
            match self.codec {
                None => self.codec = Some(au.codec),
                Some(expected) if expected != au.codec => {
                    return Err(SinkError::CodecChanged {
                        expected,
                        got: au.codec,
                    });
                }
                Some(_) => {}
            }
            let n = write_annexb(writer, &au).await?;
            self.frames += 1;
            self.bytes += n as u64;
            Ok(())
        })
    }

    fn flush(&mut self) -> SinkFuture<'_> {
        Box::pin(async move {
            let writer = self.writer.as_mut().ok_or(SinkError::Closed)?;
            writer.flush().await?;
            Ok(())
        })
    }

    fn close(&mut self) -> SinkFuture<'_> {
        Box::pin(async move {
            let Some(mut writer) = self.writer.take() else {
                return Ok(());
            };
            writer.flush().await?;
            writer.get_ref().sync_all().await?;
            Ok(())
        })
    }
}
