//! Surveillance Station **MixStream** WebSocket client.
//!
//! The protocol both DSM's browser Monitor Center and the official desktop
//! client (`video_plugin.dll`, `DemuxWs`) speak on
//! `/ss_webstream_task/?method=MixStream…`. Protocol knowledge comes from
//! the reference Python client (`ssc-gtk`, `services/ws_bridge.py`,
//! `services/live.py`, `services/aac.py`) and from the decompiled desktop
//! client (`docs/architecture.md` §3.2, §5, §6).
//!
//! # Layers
//!
//! | module | what |
//! |---|---|
//! | [`url`] | connect URLs: [`live_url`], [`history_url`] |
//! | [`frame`] | [`parse_frame`]: 4-byte BE header length, `k=v&…` header, payload; [`Frame::annex_b`] |
//! | [`command`] | in-band text commands: [`command::history_play`], [`command::seek_ms`], [`HistorySession`], keepalive constants |
//! | [`aac`] | AAC frame reconstruction and ADTS headers for `adoCodec=MPEG4-GENERIC` |
//! | [`transport`] | the [`Transport`]/[`Connector`] traits and [`TungsteniteConnector`] |
//! | [`client`] | [`MixStreamClient`]: connect, deliver [`Event`]s, keepalive, stall detection, reconnect |
//!
//! # Authentication
//!
//! Only a `Cookie: id=<sid>` header on the WebSocket upgrade; no `_sid`
//! query parameter. This crate never logs or embeds the sid in errors.
//!
//! # Example
//!
//! ```no_run
//! use ss_stream::{Event, FrameKind, MixStreamClient, MixStreamConfig, StreamProfile, TungsteniteConnector};
//!
//! # async fn demo(sid: String) -> Result<(), Box<dyn std::error::Error>> {
//! let url = ss_stream::live_url("https://nas:5001", 3, StreamProfile::Balanced)?;
//! let (handle, mut events) = MixStreamClient::connect(
//!     MixStreamConfig::live(url, sid),
//!     TungsteniteConnector::insecure(), // self-signed NAS certificate
//! );
//! while let Some(event) = events.recv().await {
//!     match event {
//!         Event::Frame(frame) if frame.kind() == FrameKind::Video => {
//!             let access_unit = frame.annex_b(); // 00 00 00 01 + NAL bytes, feed to a decoder
//!             let _ = access_unit;
//!         }
//!         Event::Frame(frame) => { let _ = frame.kind(); /* codec info, audio, close notice */ }
//!         Event::GaveUp(reason) => { eprintln!("stream lost: {reason}"); break; }
//!         _ => {}
//!     }
//! }
//! handle.stop().await;
//! # Ok(()) }
//! ```
//!
//! History playback: build the URL with [`history_url`], the parameters
//! with [`PlayParams`] (`start`/`end` in seconds **into the recording**),
//! use [`MixStreamConfig::history`], then drive it with
//! [`MixStreamHandle::seek_ms`], [`MixStreamHandle::pause`],
//! [`MixStreamHandle::set_speed`], [`MixStreamHandle::set_reverse`] and
//! [`MixStreamHandle::play`] (another recording). Each video frame's
//! [`Header::msec`] is the position DSM has actually reached.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod aac;
pub mod client;
pub mod command;
pub mod error;
pub mod frame;
pub mod transport;
pub mod url;

pub use client::{
    Disconnect, Event, MixStreamClient, MixStreamConfig, MixStreamHandle, StreamMode,
};
pub use command::{
    HistorySession, KEEPALIVE_INTERVAL, KEEPALIVE_MESSAGE, PlayParams, Speed, StampCounter,
};
pub use error::{ClientClosed, FrameError, TransportError, UrlError};
pub use frame::{
    ANNEX_B_START_CODE, AudioCodec, Frame, FrameKind, Header, MediaType, VideoCodec, encode_frame,
    parse_frame, parse_frame_bytes,
};
pub use transport::{Connector, Transport, TungsteniteConnector, WsMessage};
pub use url::{StreamProfile, history_url, live_url};
