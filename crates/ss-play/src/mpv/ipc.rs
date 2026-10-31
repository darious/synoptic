//! mpv's JSON IPC (`--input-ipc-server=<unix socket>`): newline-delimited
//! JSON, `{"command":[…],"request_id":n}` in, `{"request_id":n,"error":
//! "success","data":…}` out, with asynchronous `{"event":…}` lines mixed
//! in. See `mpv(1)` "JSON IPC".

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::sink::SinkError;

/// Reference point for [`MpvIpc::seek`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekMode {
    /// Seconds relative to the current position.
    Relative,
    /// Absolute position in seconds.
    Absolute,
    /// Absolute position as a percentage of the duration.
    AbsolutePercent,
}

impl SeekMode {
    fn as_str(self) -> &'static str {
        match self {
            SeekMode::Relative => "relative",
            SeekMode::Absolute => "absolute",
            SeekMode::AbsolutePercent => "absolute-percent",
        }
    }
}

/// A connection to one mpv's IPC socket. One command in flight at a time;
/// events that arrive while waiting for a reply are kept in a bounded
/// queue, readable with [`MpvIpc::take_events`].
pub struct MpvIpc {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
    events: Vec<Value>,
    line: String,
}

/// Events kept before the oldest is dropped.
const MAX_EVENTS: usize = 256;

impl MpvIpc {
    /// Connect to `path`, retrying until `timeout` elapses: mpv creates the
    /// socket shortly after start, not synchronously with it.
    pub async fn connect(path: &Path, timeout: Duration) -> Result<Self, SinkError> {
        let deadline = Instant::now() + timeout;
        loop {
            match UnixStream::connect(path).await {
                Ok(stream) => return Ok(Self::from_stream(stream)),
                Err(e) => {
                    if Instant::now() >= deadline {
                        return Err(SinkError::Ipc(format!(
                            "could not connect to {} within {timeout:?}: {e}",
                            path.display()
                        )));
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        }
    }

    fn from_stream(stream: UnixStream) -> Self {
        let (r, w) = stream.into_split();
        Self {
            reader: BufReader::new(r),
            writer: w,
            next_id: 1,
            events: Vec::new(),
            line: String::new(),
        }
    }

    /// Send a raw command (`["set_property","pause",true]`) and return its
    /// `data` (JSON `null` when the command has none).
    pub async fn command(&mut self, args: &[Value]) -> Result<Value, SinkError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut msg = serde_json::to_vec(&json!({"command": args, "request_id": id}))?;
        msg.push(b'\n');
        self.writer
            .write_all(&msg)
            .await
            .map_err(|e| SinkError::Ipc(format!("write: {e}")))?;
        loop {
            self.line.clear();
            let n = self
                .reader
                .read_line(&mut self.line)
                .await
                .map_err(|e| SinkError::Ipc(format!("read: {e}")))?;
            if n == 0 {
                return Err(SinkError::Ipc("mpv closed the IPC socket".into()));
            }
            let trimmed = self.line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let reply: Value = serde_json::from_str(trimmed)?;
            if reply.get("request_id").and_then(Value::as_u64) == Some(id) {
                let error = reply.get("error").and_then(Value::as_str).unwrap_or("");
                if error != "success" {
                    return Err(SinkError::IpcCommand {
                        command: Value::Array(args.to_vec()).to_string(),
                        error: error.to_string(),
                    });
                }
                return Ok(reply.get("data").cloned().unwrap_or(Value::Null));
            }
            if reply.get("event").is_some() {
                if self.events.len() == MAX_EVENTS {
                    self.events.remove(0);
                }
                self.events.push(reply);
            }
        }
    }

    /// Events received so far (while waiting for replies), oldest first.
    pub fn take_events(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.events)
    }

    /// `set_property name value`.
    pub async fn set_property(
        &mut self,
        name: &str,
        value: impl Into<Value>,
    ) -> Result<(), SinkError> {
        self.command(&[json!("set_property"), json!(name), value.into()])
            .await
            .map(drop)
    }

    /// `get_property name`.
    pub async fn get_property(&mut self, name: &str) -> Result<Value, SinkError> {
        self.command(&[json!("get_property"), json!(name)]).await
    }

    /// Pause or resume rendering. Note that mpv keeps reading its input
    /// while paused (its demuxer fills the cache up to `demuxer-max-bytes`
    /// and then stops draining the pipe): a producer that pauses a live
    /// pipe for long should stop writing, or expect
    /// [`SinkError::WriteStalled`].
    pub async fn set_pause(&mut self, paused: bool) -> Result<(), SinkError> {
        self.set_property("pause", paused).await
    }

    /// Current `pause` state.
    pub async fn paused(&mut self) -> Result<bool, SinkError> {
        Ok(self.get_property("pause").await?.as_bool().unwrap_or(false))
    }

    /// `seek <seconds> <mode>`. Meaningless on a pipe (mpv answers success
    /// and does nothing); useful for recordings played by URL.
    pub async fn seek(&mut self, seconds: f64, mode: SeekMode) -> Result<(), SinkError> {
        self.command(&[json!("seek"), json!(seconds), json!(mode.as_str())])
            .await
            .map(drop)
    }

    /// Jump to the live edge: throw away everything mpv has buffered
    /// (`drop-buffers`) and resume. On a pipe the next frame mpv shows is
    /// the next one written, so the producer should drop its own backlog
    /// too and ideally restart at a keyframe.
    pub async fn seek_to_live(&mut self) -> Result<(), SinkError> {
        self.command(&[json!("drop-buffers")]).await?;
        self.set_pause(false).await
    }

    /// Seconds of demuxer cache currently buffered (`demuxer-cache-duration`),
    /// `None` when mpv has no estimate.
    pub async fn cache_duration(&mut self) -> Result<Option<f64>, SinkError> {
        Ok(self.get_property("demuxer-cache-duration").await?.as_f64())
    }

    /// Playback speed multiplier (the reference nudges this between 0.8 and
    /// 1.2 to hold the cache near its target).
    pub async fn set_speed(&mut self, speed: f64) -> Result<(), SinkError> {
        self.set_property("speed", speed).await
    }

    /// Ask mpv to exit (exit status 0).
    pub async fn quit(&mut self) -> Result<(), SinkError> {
        match self.command(&[json!("quit")]).await {
            Ok(_) => Ok(()),
            // mpv may close the socket before the reply makes it out.
            Err(SinkError::Ipc(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }
}
