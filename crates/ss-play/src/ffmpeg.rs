//! [`FfmpegRemuxSink`]: record an Annex-B stream to a container file by
//! piping it through `ffmpeg -f h264|hevc -i pipe:0 -c copy <out>`.
//!
//! No transcoding happens; ffmpeg only parses NAL boundaries and writes
//! MP4/Matroska packets. What it cannot do is take per-frame timestamps from
//! us (a raw elementary stream has none), hence [`Timestamps`]:
//!
//! * [`Timestamps::Wallclock`] stamps each packet with the time it reached
//!   ffmpeg (`-use_wallclock_as_timestamps 1`), which is correct for a live
//!   stream written as it arrives and what the reference client uses for its
//!   live mux. Default.
//! * [`Timestamps::ConstantRate`] assumes a fixed frame rate and numbers the
//!   frames (`-framerate` + `setts=ts=N/<fps>/TB`): right for a stream dumped
//!   faster than real time (tests, re-muxing a [`RawFileSink`] file) and for
//!   cameras with a known fixed rate.
//!
//! Measured on ffmpeg 9.0.2: plain `-f h264 -i pipe:0 -c copy out.mkv` is
//! refused outright ("Can't write packet with unknown timestamp") and the MP4
//! muxer drops frames with "pts has no value", so one of the two modes is
//! always applied. `-fflags +genpts` and an input `-r` do not help.
//!
//! [`RawFileSink`]: crate::RawFileSink

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;

use crate::process::{ChildProcess, grow_pipe_buffer};
use crate::sink::{AccessUnit, Codec, FrameSink, SinkError, SinkFuture, write_annexb};

/// How ffmpeg assigns timestamps to packets of a raw elementary stream.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Timestamps {
    /// Arrival time at ffmpeg's stdin. For live recording.
    Wallclock,
    /// Fixed frame rate in frames per second; frames are numbered.
    ConstantRate(f64),
}

/// Settings for [`FfmpegRemuxSink`].
#[derive(Clone, Debug)]
pub struct FfmpegRemuxOptions {
    /// `ffmpeg` executable (name on `PATH` or a path).
    pub binary: PathBuf,
    /// Output file. The container is chosen by extension (`.mp4`, `.mkv`,
    /// …) unless `container` is set.
    pub output: PathBuf,
    /// Explicit output format (`-f`), e.g. `"matroska"`.
    pub container: Option<String>,
    /// Timestamp strategy, see [`Timestamps`].
    pub timestamps: Timestamps,
    /// Extra arguments inserted before `-i pipe:0` (input options).
    pub extra_input_args: Vec<String>,
    /// Extra arguments inserted before the output path (output options),
    /// e.g. `-movflags +frag_keyframe+empty_moov` for a crash-tolerant MP4.
    pub extra_output_args: Vec<String>,
    /// Overwrite an existing output (`-y`) instead of failing (`-n`).
    pub overwrite: bool,
    /// `-loglevel` passed to ffmpeg; its stderr tail is attached to errors.
    pub loglevel: String,
    /// How long one pipe write may block before the sink gives up on a
    /// stalled ffmpeg ([`SinkError::WriteStalled`]).
    pub write_timeout: Duration,
    /// How long `close` waits for ffmpeg to finalize the file after stdin
    /// closes before killing it.
    pub exit_timeout: Duration,
}

impl FfmpegRemuxOptions {
    /// Defaults for recording a live stream to `output`.
    pub fn new(output: impl Into<PathBuf>) -> Self {
        Self {
            binary: PathBuf::from("ffmpeg"),
            output: output.into(),
            container: None,
            timestamps: Timestamps::Wallclock,
            extra_input_args: Vec::new(),
            extra_output_args: Vec::new(),
            overwrite: true,
            loglevel: "warning".into(),
            write_timeout: Duration::from_secs(5),
            exit_timeout: Duration::from_secs(30),
        }
    }

    /// Set the timestamp strategy.
    pub fn timestamps(mut self, timestamps: Timestamps) -> Self {
        self.timestamps = timestamps;
        self
    }

    /// Use a specific ffmpeg executable.
    pub fn binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = binary.into();
        self
    }

    /// The full argument list for a stream of `codec`.
    pub fn args(&self, codec: Codec) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-hide_banner".into(),
            "-nostdin".into(),
            "-loglevel".into(),
            self.loglevel.clone(),
        ];
        match self.timestamps {
            Timestamps::Wallclock => {
                args.extend(["-use_wallclock_as_timestamps".into(), "1".into()]);
            }
            Timestamps::ConstantRate(fps) => {
                args.extend(["-framerate".into(), format!("{fps}")]);
            }
        }
        args.extend(self.extra_input_args.iter().cloned());
        args.extend([
            "-f".into(),
            codec.ffmpeg_format().into(),
            "-i".into(),
            "pipe:0".into(),
        ]);
        args.extend(["-c".into(), "copy".into()]);
        if let Timestamps::ConstantRate(fps) = self.timestamps {
            // libavformat's raw demuxers leave packets unstamped on this
            // ffmpeg; number the frames ourselves. `FR` is not a setts
            // constant, so the rate is baked into the expression.
            args.extend(["-bsf:v".into(), format!("setts=ts=N/{fps}/TB")]);
        }
        if let Some(container) = &self.container {
            args.extend(["-f".into(), container.clone()]);
        }
        args.extend(self.extra_output_args.iter().cloned());
        args.push(if self.overwrite {
            "-y".into()
        } else {
            "-n".into()
        });
        args.push(self.output.display().to_string());
        args
    }
}

/// Remuxes an Annex-B stream into a container file via an ffmpeg child.
///
/// ffmpeg is started lazily on the first `push` (the `-f` input format
/// needs the codec), or eagerly with [`FfmpegRemuxSink::start`].
pub struct FfmpegRemuxSink {
    options: FfmpegRemuxOptions,
    codec: Option<Codec>,
    child: Option<ChildProcess>,
    stdin: Option<ChildStdin>,
    closed: bool,
    frames: u64,
    bytes: u64,
}

impl FfmpegRemuxSink {
    /// Record to `output` with default options (wall-clock timestamps).
    pub fn new(output: impl Into<PathBuf>) -> Self {
        Self::with_options(FfmpegRemuxOptions::new(output))
    }

    /// Record with explicit options.
    pub fn with_options(options: FfmpegRemuxOptions) -> Self {
        Self {
            options,
            codec: None,
            child: None,
            stdin: None,
            closed: false,
            frames: 0,
            bytes: 0,
        }
    }

    /// Spawn ffmpeg now for a stream of `codec`.
    pub async fn start(&mut self, codec: Codec) -> Result<(), SinkError> {
        if self.closed {
            return Err(SinkError::Closed);
        }
        if let Some(expected) = self.codec {
            return if expected == codec {
                Ok(())
            } else {
                Err(SinkError::CodecChanged {
                    expected,
                    got: codec,
                })
            };
        }
        let mut child = ChildProcess::spawn(
            &self.options.binary,
            self.options.args(codec),
            Stdio::piped(),
            true,
        )?;
        let stdin = child.take_stdin().ok_or(SinkError::Closed)?;
        grow_pipe_buffer(&stdin);
        self.child = Some(child);
        self.stdin = Some(stdin);
        self.codec = Some(codec);
        Ok(())
    }

    /// Output path.
    pub fn output(&self) -> &Path {
        &self.options.output
    }

    /// The options in effect.
    pub fn options(&self) -> &FfmpegRemuxOptions {
        &self.options
    }

    /// Access units pushed so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Bytes piped so far (start codes included).
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    /// ffmpeg's pid while it runs.
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(ChildProcess::id)
    }

    async fn fail_if_exited(&mut self) -> Result<(), SinkError> {
        if let Some(child) = self.child.as_mut()
            && let Some(status) = child.try_wait()?
        {
            self.stdin = None;
            self.closed = true;
            return child.check(status).await.and(Err(SinkError::Closed));
        }
        Ok(())
    }
}

impl FrameSink for FfmpegRemuxSink {
    fn push(&mut self, au: AccessUnit) -> SinkFuture<'_> {
        Box::pin(async move {
            if self.closed {
                return Err(SinkError::Closed);
            }
            self.start(au.codec).await?;
            let timeout = self.options.write_timeout;
            let stdin = self.stdin.as_mut().ok_or(SinkError::Closed)?;
            match tokio::time::timeout(timeout, write_annexb(stdin, &au)).await {
                Ok(Ok(n)) => {
                    self.frames += 1;
                    self.bytes += n as u64;
                    Ok(())
                }
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                    // ffmpeg went away; report why rather than EPIPE.
                    self.fail_if_exited().await?;
                    Err(e.into())
                }
                Ok(Err(e)) => Err(e.into()),
                Err(_) => Err(SinkError::WriteStalled(timeout)),
            }
        })
    }

    fn flush(&mut self) -> SinkFuture<'_> {
        Box::pin(async move {
            if self.closed {
                return Err(SinkError::Closed);
            }
            if let Some(stdin) = self.stdin.as_mut() {
                stdin.flush().await?;
            }
            Ok(())
        })
    }

    fn close(&mut self) -> SinkFuture<'_> {
        Box::pin(async move {
            if self.closed {
                return Ok(());
            }
            self.closed = true;
            // EOF on stdin is what makes ffmpeg write the trailer (moov).
            drop(self.stdin.take());
            let Some(mut child) = self.child.take() else {
                return Ok(());
            };
            let status = child.wait_or_kill(self.options.exit_timeout).await?;
            child.check(status).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallclock_args() {
        let args = FfmpegRemuxOptions::new("/x/out.mp4").args(Codec::H264);
        let joined = args.join(" ");
        assert!(
            joined
                .contains("-use_wallclock_as_timestamps 1 -f h264 -i pipe:0 -c copy -y /x/out.mp4"),
            "{joined}"
        );
        assert!(!joined.contains("setts"));
    }

    #[test]
    fn constant_rate_args_number_frames() {
        let args = FfmpegRemuxOptions::new("out.mkv")
            .timestamps(Timestamps::ConstantRate(10.0))
            .args(Codec::H265);
        let joined = args.join(" ");
        assert!(
            joined.contains(
                "-framerate 10 -f hevc -i pipe:0 -c copy -bsf:v setts=ts=N/10/TB -y out.mkv"
            ),
            "{joined}"
        );
    }

    #[test]
    fn explicit_container_and_extras() {
        let mut opts = FfmpegRemuxOptions::new("out.bin");
        opts.container = Some("matroska".into());
        opts.extra_output_args = vec!["-movflags".into(), "+faststart".into()];
        opts.overwrite = false;
        let joined = opts.args(Codec::H264).join(" ");
        assert!(
            joined.ends_with("-c copy -f matroska -movflags +faststart -n out.bin"),
            "{joined}"
        );
    }
}
