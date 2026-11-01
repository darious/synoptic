//! Shared subprocess plumbing: spawning with a clear "not installed" error,
//! capturing a stderr tail, Linux pipe-buffer sizing, and a bounded wait.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::task::JoinHandle;

use crate::sink::SinkError;

/// How many stderr lines a [`ChildProcess`] keeps for error reports.
const STDERR_TAIL_LINES: usize = 40;

/// Pipe buffer the sinks ask the kernel for on a child's stdin
/// (`F_SETPIPE_SZ`). Linux defaults to 64 KiB, which a couple of H.265
/// keyframes fill before mpv has even opened its demuxer; 1 MiB is the
/// unprivileged ceiling on a stock kernel (`/proc/sys/fs/pipe-max-size`).
pub const PIPE_CAPACITY: usize = 1024 * 1024;

/// Locate `name` on `PATH` (or verify an absolute/relative path exists).
/// Tests use it to skip when a helper binary is missing.
pub fn find_binary(name: impl AsRef<Path>) -> Option<PathBuf> {
    let name = name.as_ref();
    if name.components().count() > 1 {
        return name.is_file().then(|| name.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Grow the pipe behind `stdin` to [`PIPE_CAPACITY`]. Best effort: a kernel
/// that refuses (non-Linux, a lowered `pipe-max-size`) keeps the default.
#[cfg(target_os = "linux")]
pub(crate) fn grow_pipe_buffer(stdin: &ChildStdin) {
    use std::os::fd::AsRawFd;
    let fd = stdin.as_raw_fd();
    // SAFETY: fcntl(F_SETPIPE_SZ) on a descriptor we own; it only changes the
    // pipe's buffer size and touches no memory of ours.
    let rc = unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, PIPE_CAPACITY as libc::c_int) };
    if rc < 0 {
        tracing::debug!(
            "F_SETPIPE_SZ({PIPE_CAPACITY}) refused: {}",
            std::io::Error::last_os_error()
        );
    }
}

/// `F_SETPIPE_SZ` is Linux-only; elsewhere the default pipe size stays.
#[cfg(not(target_os = "linux"))]
pub(crate) fn grow_pipe_buffer(_stdin: &ChildStdin) {}

/// A spawned helper with its stderr drained into a bounded tail.
pub(crate) struct ChildProcess {
    pub(crate) binary: String,
    child: Child,
    stderr_task: Option<JoinHandle<String>>,
    status: Option<ExitStatus>,
}

impl ChildProcess {
    /// Spawn `binary args…` with the given stdin. stdout is discarded;
    /// stderr is captured when `capture_stderr` is set, else discarded.
    pub(crate) fn spawn<I, S>(
        binary: &Path,
        args: I,
        stdin: Stdio,
        capture_stderr: bool,
    ) -> Result<Self, SinkError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let name = binary.display().to_string();
        let mut cmd = Command::new(binary);
        cmd.args(args)
            .stdin(stdin)
            .stdout(Stdio::null())
            .stderr(if capture_stderr {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(SinkError::BinaryNotFound { binary: name });
            }
            Err(e) => return Err(e.into()),
        };
        let stderr_task = child.stderr.take().map(|stderr| {
            let label = name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                let mut tail: VecDeque<String> = VecDeque::with_capacity(STDERR_TAIL_LINES);
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "ss_play::child", "{label}: {line}");
                    if tail.len() == STDERR_TAIL_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
                tail.into_iter().collect::<Vec<_>>().join("\n")
            })
        });
        tracing::debug!("spawned {name} (pid {:?})", child.id());
        Ok(Self {
            binary: name,
            child,
            stderr_task,
            status: None,
        })
    }

    pub(crate) fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    pub(crate) fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Exit status if the child has already exited.
    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        let status = self.child.try_wait()?;
        self.status = status;
        Ok(status)
    }

    /// Wait for exit. Cancel-safe (`Child::wait` is).
    pub(crate) async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let status = self.child.wait().await?;
        self.status = Some(status);
        Ok(status)
    }

    /// Wait up to `timeout`, then SIGKILL and reap.
    pub(crate) async fn wait_or_kill(&mut self, timeout: Duration) -> std::io::Result<ExitStatus> {
        match tokio::time::timeout(timeout, self.wait()).await {
            Ok(status) => status,
            Err(_) => {
                tracing::warn!(
                    "{} did not exit within {timeout:?}, killing it",
                    self.binary
                );
                self.kill().await
            }
        }
    }

    /// SIGKILL and reap.
    pub(crate) async fn kill(&mut self) -> std::io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        self.child.start_kill()?;
        self.wait().await
    }

    /// The captured stderr tail (empty if not captured or still running).
    pub(crate) async fn stderr_tail(&mut self) -> String {
        match self.stderr_task.take() {
            Some(task) => task.await.unwrap_or_default(),
            None => String::new(),
        }
    }

    /// Map an exit status to `Ok(())`/[`SinkError::ProcessFailed`].
    pub(crate) async fn check(&mut self, status: ExitStatus) -> Result<(), SinkError> {
        if status.success() {
            Ok(())
        } else {
            Err(SinkError::ProcessFailed {
                binary: self.binary.clone(),
                status,
                stderr: self.stderr_tail().await,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_binary_searches_path() {
        assert!(find_binary("definitely-not-a-real-binary-ss-play").is_none());
        // `sh` exists on every POSIX box this crate can run on.
        assert!(find_binary("sh").is_some());
        assert_eq!(
            find_binary("/bin/sh"),
            Path::new("/bin/sh")
                .is_file()
                .then(|| PathBuf::from("/bin/sh"))
        );
    }
}
