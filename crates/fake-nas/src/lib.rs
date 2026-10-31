//! `fake-nas`: an offline stand-in for a Synology DSM / Surveillance
//! Station NAS, good enough for the `synoptic` GTK app and the `ss` CLI to
//! log in, list cameras, play live and recorded video over MixStream and
//! browse recordings, events, snapshots, alerts, licences and time-lapse
//! tasks on localhost.
//!
//! Correctness relative to what *our* clients (`ss-api`, `ss-stream`)
//! parse matters more than fidelity to Synology: shapes come from their
//! models and fixtures. See [`api`] for the HTTP side, [`stream`] for the
//! WebSocket, [`world`] for the synthetic data and [`media`] for how the
//! fixture H.264/AAC is cut into frames.

#![forbid(unsafe_code)]

pub mod api;
pub mod media;
pub mod rng;
pub mod stream;
pub mod world;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use tokio::net::TcpListener;

pub use api::App;
pub use media::{Media, MediaError};
pub use world::World;

/// Server settings.
#[derive(Clone, Debug)]
pub struct Config {
    pub user: String,
    pub password: String,
    /// When set, a login without this `otp_code` fails with the
    /// two-factor codes `ss_api` maps to `Error::OtpRequired`.
    pub otp: Option<String>,
    /// Directory holding `test.h264`, `test.aac`, `test.mp4` (and
    /// `snap.jpg`, created when missing).
    pub fixtures: PathBuf,
    /// Seed of the synthetic world.
    pub seed: u64,
    /// Port quoted in the URLs the API hands out (RTSP-over-HTTP path).
    pub advertised_port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            user: "admin".into(),
            password: "admin".into(),
            otp: None,
            fixtures: default_fixtures_dir(),
            seed: 1,
            advertised_port: 5000,
        }
    }
}

/// `~/.cache/synoptic/fixtures`.
pub fn default_fixtures_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("synoptic").join("fixtures")
}

/// The axum router: `/webapi/<cgi>` and `/ss_webstream_task/`.
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { "fake-nas: Synology Surveillance Station stand-in" }),
        )
        .route("/webapi/{cgi}", get(api::webapi).post(api::webapi))
        .route("/ss_webstream_task/", get(stream::ws_handler))
        .route("/ss_webstream_task", get(stream::ws_handler))
        .with_state(app)
}

/// Serve `app` on `listener` until the task is dropped.
pub async fn serve(app: Arc<App>, listener: TcpListener) -> std::io::Result<()> {
    axum::serve(listener, router(app)).await
}
