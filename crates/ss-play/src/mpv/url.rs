//! [`MpvUrlPlayer`]: let mpv fetch an RTSP/HTTP URL (or open a local
//! recording) itself, with the reference's RTSP settings.

use std::process::{ExitStatus, Stdio};

use crate::mpv::ipc::{MpvIpc, SeekMode};
use crate::mpv::options::MpvOptions;
use crate::mpv::process::MpvProcess;
use crate::sink::SinkError;

/// An mpv playing a URL. Not a [`FrameSink`](crate::FrameSink): there is
/// nothing to push, mpv pulls.
pub struct MpvUrlPlayer {
    process: MpvProcess,
    url: String,
}

impl MpvUrlPlayer {
    /// Play `url` with the [`PlaybackProfile::Default`] profile
    /// (`rtsp_transport=tcp`, 0.5 s cache) and the environment overrides.
    ///
    /// [`PlaybackProfile::Default`]: crate::PlaybackProfile::Default
    pub fn spawn(url: &str) -> Result<Self, SinkError> {
        Self::spawn_with(MpvOptions::url().from_env(), url)
    }

    /// Play `url` with explicit options. stdin is `/dev/null`.
    pub fn spawn_with(options: MpvOptions, url: &str) -> Result<Self, SinkError> {
        let process = MpvProcess::spawn(options, url, Stdio::null())?;
        Ok(Self {
            process,
            url: url.to_string(),
        })
    }

    /// The URL being played.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The underlying process.
    pub fn process(&self) -> &MpvProcess {
        &self.process
    }

    /// Mutable access to the process.
    pub fn process_mut(&mut self) -> &mut MpvProcess {
        &mut self.process
    }

    /// The IPC connection, established on first use.
    pub async fn ipc(&mut self) -> Result<&mut MpvIpc, SinkError> {
        self.process.ipc().await
    }

    /// Pause or resume.
    pub async fn set_pause(&mut self, paused: bool) -> Result<(), SinkError> {
        self.process.set_pause(paused).await
    }

    /// Seek within a recording.
    pub async fn seek(&mut self, seconds: f64, mode: SeekMode) -> Result<(), SinkError> {
        self.ipc().await?.seek(seconds, mode).await
    }

    /// Drop buffers and resume (live RTSP: back to the newest frame).
    pub async fn seek_to_live(&mut self) -> Result<(), SinkError> {
        self.process.seek_to_live().await
    }

    /// Ask mpv to quit and wait for it.
    pub async fn quit(&mut self) -> Result<ExitStatus, SinkError> {
        self.process.quit().await
    }

    /// Wait for mpv to exit on its own.
    pub async fn wait(&mut self) -> Result<ExitStatus, SinkError> {
        self.process.wait().await
    }

    /// Wait and map a non-zero exit to [`SinkError::ProcessFailed`].
    pub async fn wait_checked(&mut self) -> Result<(), SinkError> {
        let status = self.process.wait().await?;
        self.process.check(status).await
    }
}
