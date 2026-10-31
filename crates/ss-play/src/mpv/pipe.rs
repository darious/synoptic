//! [`MpvPipeSink`]: display a raw Annex-B stream by feeding it to an `mpv`
//! child over its stdin (`fd://0`).

use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;

use crate::mpv::ipc::MpvIpc;
use crate::mpv::options::{MpvOptions, PlaybackProfile};
use crate::mpv::process::MpvProcess;
use crate::process::grow_pipe_buffer;
use crate::sink::{AccessUnit, Codec, FrameSink, SinkError, SinkFuture, write_annexb};

/// An mpv window (or `--vo=null`) fed from a pipe.
pub struct MpvPipeSink {
    process: MpvProcess,
    stdin: Option<ChildStdin>,
    codec: Codec,
    write_timeout: Duration,
    closed: bool,
    frames: u64,
    bytes: u64,
}

impl MpvPipeSink {
    /// Start mpv for a stream of `codec` with the
    /// [`PlaybackProfile::LowLatency`] profile.
    pub fn spawn(codec: Codec) -> Result<Self, SinkError> {
        Self::spawn_with(MpvOptions::low_latency(codec).from_env(), codec)
    }

    /// Start mpv with explicit options. For the low-latency profile the
    /// options' `video_format` is set to `codec` so libavformat is told the
    /// raw format up front.
    pub fn spawn_with(mut options: MpvOptions, codec: Codec) -> Result<Self, SinkError> {
        if options.profile == PlaybackProfile::LowLatency {
            options.video_format = Some(codec);
        }
        let mut process = MpvProcess::spawn(options, "fd://0", Stdio::piped())?;
        let stdin = process.child.take_stdin().ok_or(SinkError::Closed)?;
        grow_pipe_buffer(&stdin);
        Ok(Self {
            process,
            stdin: Some(stdin),
            codec,
            write_timeout: Duration::from_secs(5),
            closed: false,
            frames: 0,
            bytes: 0,
        })
    }

    /// How long one pipe write may block before the sink reports
    /// [`SinkError::WriteStalled`] (default 5 s).
    pub fn set_write_timeout(&mut self, timeout: Duration) {
        self.write_timeout = timeout;
    }

    /// The underlying process (IPC socket path, pid, options).
    pub fn process(&self) -> &MpvProcess {
        &self.process
    }

    /// Mutable access to the process (for `wait`, `kill`, …).
    pub fn process_mut(&mut self) -> &mut MpvProcess {
        &mut self.process
    }

    /// The IPC connection, established on first use.
    pub async fn ipc(&mut self) -> Result<&mut MpvIpc, SinkError> {
        self.process.ipc().await
    }

    /// Pause or resume rendering.
    pub async fn set_pause(&mut self, paused: bool) -> Result<(), SinkError> {
        self.process.set_pause(paused).await
    }

    /// Drop mpv's buffers and resume so the next frame written is the next
    /// one shown.
    pub async fn seek_to_live(&mut self) -> Result<(), SinkError> {
        self.process.seek_to_live().await
    }

    /// Ask mpv to quit and wait for it. Also closes the pipe.
    pub async fn quit(&mut self) -> Result<ExitStatus, SinkError> {
        self.closed = true;
        drop(self.stdin.take());
        self.process.quit().await
    }

    /// Close only the write end of the pipe, leaving mpv running (with
    /// `keep_open` it shows the last frame; without, it exits).
    pub fn close_input(&mut self) {
        drop(self.stdin.take());
    }

    /// Wait for mpv to exit on its own.
    pub async fn wait(&mut self) -> Result<ExitStatus, SinkError> {
        self.process.wait().await
    }

    /// Codec this sink was started for.
    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Access units pushed so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Bytes piped so far (start codes included).
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }
}

impl FrameSink for MpvPipeSink {
    fn push(&mut self, au: AccessUnit) -> SinkFuture<'_> {
        Box::pin(async move {
            if au.codec != self.codec {
                return Err(SinkError::CodecChanged {
                    expected: self.codec,
                    got: au.codec,
                });
            }
            let timeout = self.write_timeout;
            let stdin = self.stdin.as_mut().ok_or(SinkError::Closed)?;
            match tokio::time::timeout(timeout, write_annexb(stdin, &au)).await {
                Ok(Ok(n)) => {
                    self.frames += 1;
                    self.bytes += n as u64;
                    Ok(())
                }
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                    // mpv is gone (window closed, decoder error): say so.
                    self.stdin = None;
                    if let Some(status) = self.process.try_wait()? {
                        return Err(SinkError::ProcessFailed {
                            binary: self.process.child.binary.clone(),
                            status,
                            stderr: String::new(),
                        });
                    }
                    Err(e.into())
                }
                Ok(Err(e)) => Err(e.into()),
                Err(_) => Err(SinkError::WriteStalled(timeout)),
            }
        })
    }

    fn flush(&mut self) -> SinkFuture<'_> {
        Box::pin(async move {
            let stdin = self.stdin.as_mut().ok_or(SinkError::Closed)?;
            stdin.flush().await?;
            Ok(())
        })
    }

    /// End the stream and wait for mpv: with `keep_open = false` it exits
    /// at EOF by itself; with `keep_open = true` it is told to `quit`.
    /// Returns [`SinkError::ProcessFailed`] on a non-zero exit.
    fn close(&mut self) -> SinkFuture<'_> {
        Box::pin(async move {
            if self.closed {
                return Ok(());
            }
            self.closed = true;
            drop(self.stdin.take());
            let status = if self.process.options().keep_open {
                self.process.quit().await?
            } else {
                self.process
                    .wait_or_kill(crate::mpv::process::EXIT_TIMEOUT)
                    .await?
            };
            self.process.check(status).await
        })
    }
}
