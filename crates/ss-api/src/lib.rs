//! Synology DSM / Surveillance Station Web API client.
//!
//! Protocol knowledge comes from `docs/architecture.md` and from the BSD-2
//! reference client credited in `NOTICE`; this crate ports its behaviour
//! (API discovery, login v6 with OTP and trusted-device tokens, the three
//! error-code tables, the camera / recording / event / snapshot / PTZ calls
//! and their response quirks) as idiomatic async Rust on `reqwest` + `rustls`.
//! It never opens a WebSocket: the MixStream transport lives in `ss-stream`
//! and only needs [`Client::base_url`] and [`Client::sid`] from here.
//!
//! # Usage
//!
//! ```no_run
//! use ss_api::{Client, Error, LiveProtocol, LoginOptions, RecordingFilter};
//!
//! # async fn run() -> ss_api::Result<()> {
//! let client = Client::builder("https://nas.local:5001")
//!     .accept_invalid_certs(true)
//!     .build()?;
//!
//! // Optional but recommended: learn each API's CGI path and max version.
//! client.discover_apis().await?;
//!
//! let options = LoginOptions {
//!     otp_code: None,
//!     enable_device_token: true,
//!     device_name: Some("my-laptop".into()),
//!     ..Default::default()
//! };
//! let session = match client.login("admin", "password", &options).await {
//!     Ok(s) => s,
//!     Err(Error::OtpRequired(e)) => {
//!         // Ask the user for the code and log in again with
//!         // `LoginOptions { otp_code: Some(code), .. }`.
//!         return Err(Error::OtpRequired(e));
//!     }
//!     Err(e) => return Err(e),
//! };
//! // `session.did` is the trusted-device token; keep it to skip OTP next time.
//! let _ = session.did;
//!
//! let info = client.ss_info().await?;
//! println!("Surveillance Station {:?} on {}", info.version, info.ds_model_name);
//!
//! for cam in client.list_cameras().await? {
//!     println!("{} {} ({:?})", cam.id, cam.name, cam.status);
//! }
//! let rtsp = client.live_view_url(1, LiveProtocol::Rtsp).await?;
//! let profile = client.live_view_profile(1).await.unwrap_or_default();
//! println!("rtsp: {rtsp}, websocket profile={}", profile.number());
//!
//! let page = client
//!     .list_recordings(&RecordingFilter::camera(1).page(0, 20))
//!     .await?;
//! if let Some(rec) = page.items.first() {
//!     let mut file = tokio::fs::File::create("rec.mp4").await?;
//!     client.download_recording(rec.id, &mut file).await?;
//! }
//!
//! // Anything else, untyped:
//! let raw = client
//!     .request("SYNO.SurveillanceStation.Camera", "GetInfo", 8, &[("cameraIds", "1")])
//!     .await?;
//! let _ = raw;
//!
//! client.logout().await;
//! # Ok(()) }
//! ```
//!
//! # Errors
//!
//! Every call returns [`Result`]. The NAS's `{success:false, error:{code}}`
//! envelope is decoded into [`Error::Api`], or into the typed
//! [`Error::OtpRequired`] (`SYNO.API.Auth` 403/404/406) and
//! [`Error::SessionExpired`] (105/106/107/119, or a non-JSON login page)
//! variants. Messages come from the server, else from the per-namespace
//! tables ([`COMMON_ERRORS`], [`AUTH_ERRORS`], [`SURVEILLANCE_ERRORS`]).
//! No error message, `Debug` output or log line ever carries `_sid`, a
//! password or an OTP code.
//!
//! # Sessions
//!
//! A [`Client`] is `Clone` and shares its session between clones. With
//! [`LoginOptions::remember_credentials`] the client re-logs in once and
//! retries when a call hits a session error; without it the error is
//! returned and the caller decides.

#![forbid(unsafe_code)]

pub mod auth;
pub mod camera;
pub mod client;
mod de;
pub mod error;
pub mod event;
pub mod models;
pub mod ptz;
pub mod recording;
pub mod snapshot;
pub mod system;

pub use auth::{AUTH_API, AUTH_VERSION, LoginOptions, SESSION_NAME};
pub use client::{ApiRequest, Client, ClientBuilder, DEFAULT_TIMEOUT, DEFAULT_USER_AGENT};
pub use error::{
    AUTH_ERRORS, ApiError, COMMON_ERRORS, Error, ErrorNamespace, OTP_ERROR_CODES, Result,
    SESSION_ERROR_CODES, SURVEILLANCE_ERRORS, error_message,
};
pub use models::{
    Alert, ApiInfo, Camera, CameraStatus, Event, EventCenterCapability, EventCenterEvent,
    HomeModeInfo, IntervalCamera, IntervalRecording, License, LicenseInfo, LiveProtocol,
    LiveViewPath, Page, PtzPatrol, PtzPreset, Recording, Session, Snapshot, SsInfo, SsVersion,
    StreamProfile, TimeLapseRecording, TimeLapseTask, VideoCodec, decode_detection_labels,
    merge_intervals,
};
pub use ptz::{FocusControl, PtzDirection, ZoomControl};
pub use recording::{RECORDING_DOWNLOAD_VERSION, RecordingFilter};
pub use snapshot::SnapshotFilter;
