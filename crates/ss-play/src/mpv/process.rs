//! The mpv child shared by [`MpvPipeSink`](crate::MpvPipeSink) and
//! [`MpvUrlPlayer`](crate::MpvUrlPlayer): spawn with an IPC socket, lazy
//! IPC connection, exit handling.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::mpv::ipc::MpvIpc;
use crate::mpv::options::MpvOptions;
use crate::process::ChildProcess;
use crate::sink::SinkError;

/// How long to wait for mpv to create its IPC socket.
pub const IPC_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long `close` gives mpv to exit on its own before SIGKILL.
pub const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

static SOCKET_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique socket path under `$XDG_RUNTIME_DIR` (or the temp dir).
pub(crate) fn ipc_socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|d| d.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let n = SOCKET_COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("ss-play-mpv-{}-{n}.sock", std::process::id()))
}

/// A running mpv with its IPC socket.
pub struct MpvProcess {
    pub(crate) child: ChildProcess,
    options: MpvOptions,
    socket: PathBuf,
    ipc: Option<MpvIpc>,
}

impl MpvProcess {
    /// Spawn `mpv <options> <source>` with `stdin`.
    pub(crate) fn spawn(
        options: MpvOptions,
        source: &str,
        stdin: Stdio,
    ) -> Result<Self, SinkError> {
        let socket = ipc_socket_path();
        let _ = std::fs::remove_file(&socket);
        let mut args = options.to_args(Some(&socket));
        args.push("--".into());
        args.push(source.to_string());
        tracing::debug!("mpv args: {args:?}");
        // --no-terminal means stderr is silent anyway; don't bother piping it.
        let child = ChildProcess::spawn(&options.binary, &args, stdin, false)?;
        Ok(Self {
            child,
            options,
            socket,
            ipc: None,
        })
    }

    /// The options mpv was started with.
    pub fn options(&self) -> &MpvOptions {
        &self.options
    }

    /// Path of the JSON IPC socket (`--input-ipc-server`).
    pub fn ipc_socket_path(&self) -> &Path {
        &self.socket
    }

    /// mpv's pid while it runs.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// The IPC connection, established on first use.
    pub async fn ipc(&mut self) -> Result<&mut MpvIpc, SinkError> {
        if self.ipc.is_none() {
            if let Some(status) = self.child.try_wait()? {
                return Err(SinkError::Ipc(format!(
                    "mpv already exited with {status} before the IPC socket could be reached"
                )));
            }
            let ipc = MpvIpc::connect(&self.socket, IPC_CONNECT_TIMEOUT).await?;
            self.ipc = Some(ipc);
        }
        Ok(self.ipc.as_mut().expect("ipc just set"))
    }

    /// Pause or resume via IPC.
    pub async fn set_pause(&mut self, paused: bool) -> Result<(), SinkError> {
        self.ipc().await?.set_pause(paused).await
    }

    /// Drop buffers and resume, see [`MpvIpc::seek_to_live`].
    pub async fn seek_to_live(&mut self) -> Result<(), SinkError> {
        self.ipc().await?.seek_to_live().await
    }

    /// Ask mpv to quit via IPC and wait for it (killing it after
    /// [`EXIT_TIMEOUT`]).
    pub async fn quit(&mut self) -> Result<ExitStatus, SinkError> {
        if self.child.try_wait()?.is_none() {
            match self.ipc().await {
                Ok(ipc) => ipc.quit().await?,
                Err(e) => tracing::debug!("quit: no IPC ({e}), killing mpv"),
            }
        }
        self.wait_or_kill(EXIT_TIMEOUT).await
    }

    /// Exit status if mpv has already exited.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, SinkError> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.reaped();
        }
        Ok(status)
    }

    /// Wait for mpv to exit on its own (EOF with `keep_open = false`, the
    /// user closing the window, `quit`).
    pub async fn wait(&mut self) -> Result<ExitStatus, SinkError> {
        let status = self.child.wait().await?;
        self.reaped();
        Ok(status)
    }

    /// Wait up to `timeout`, then SIGKILL.
    pub async fn wait_or_kill(&mut self, timeout: Duration) -> Result<ExitStatus, SinkError> {
        let status = self.child.wait_or_kill(timeout).await?;
        self.reaped();
        Ok(status)
    }

    /// SIGKILL mpv and reap it.
    pub async fn kill(&mut self) -> Result<ExitStatus, SinkError> {
        let status = self.child.kill().await?;
        self.reaped();
        Ok(status)
    }

    /// Map an exit status to `Ok(())`/[`SinkError::ProcessFailed`].
    pub async fn check(&mut self, status: ExitStatus) -> Result<(), SinkError> {
        self.child.check(status).await
    }

    /// mpv 0.40 does not unlink its IPC socket on exit; do it once the
    /// process is gone, and forget the dead connection.
    fn reaped(&mut self) {
        self.ipc = None;
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl Drop for MpvProcess {
    fn drop(&mut self) {
        // Covers a sink dropped without close(): the child is killed by
        // kill_on_drop, and the socket file would otherwise stay behind.
        let _ = std::fs::remove_file(&self.socket);
    }
}
