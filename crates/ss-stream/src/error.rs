//! Error types. None of them ever carries a session id.

use std::time::Duration;

/// A base URL that cannot be turned into a `/ss_webstream_task/` endpoint.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UrlError {
    /// The base URL had no `http://`, `https://`, `ws://` or `wss://` prefix.
    #[error("base URL must start with http://, https://, ws:// or wss://")]
    UnsupportedScheme,
    /// Nothing followed the scheme.
    #[error("base URL has no host")]
    MissingHost,
}

/// A WebSocket binary message that is not a well-formed MixStream frame.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    /// Fewer than the four bytes of the header-length prefix.
    #[error("message too short for a frame: {len} bytes")]
    TooShort {
        /// Message length.
        len: usize,
    },
    /// The header-length prefix points past the end of the message.
    #[error("header length {declared} exceeds the {available} bytes after the prefix")]
    HeaderTruncated {
        /// Header length from the prefix.
        declared: usize,
        /// Bytes actually following the prefix.
        available: usize,
    },
}

/// A transport-level failure, as reported by a [`Transport`](crate::Transport).
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The connection could not be opened (TCP, TLS or the HTTP upgrade).
    #[error("connect failed: {0}")]
    Connect(String),
    /// Opening the connection took longer than allowed.
    #[error("connect timed out after {0:?}")]
    ConnectTimeout(Duration),
    /// A read or write failed on an open connection.
    #[error("connection error: {0}")]
    Io(String),
    /// The peer closed the connection (a close frame or EOF).
    #[error("connection closed")]
    Closed,
}

/// Returned by [`MixStreamHandle`](crate::MixStreamHandle) methods once the
/// client task has exited.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[error("MixStream client is no longer running")]
pub struct ClientClosed;
