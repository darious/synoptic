//! `fake-nas`: run the fake Surveillance Station on localhost.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use fake_nas::{App, Config};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "fake-nas",
    about = "Offline stand-in for a Synology Surveillance Station NAS (plain HTTP + MixStream WebSocket)",
    long_about = "Serves enough of the DSM Web API and the /ss_webstream_task/ MixStream \
                  WebSocket for the synoptic app and the ss CLI to log in, list cameras, play \
                  live and recorded video and browse recordings, events, snapshots, alerts, \
                  licences and time-lapse tasks. Everything is synthetic and deterministic; \
                  video is the fixture clip looped."
)]
struct Cli {
    /// TCP port to listen on.
    #[arg(long, default_value_t = 5000)]
    port: u16,
    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1")]
    bind: String,
    /// Directory with test.h264, test.aac, test.mp4 (snap.jpg is created
    /// there when missing) [default: ~/.cache/synoptic/fixtures].
    #[arg(long, value_name = "DIR")]
    fixtures: Option<PathBuf>,
    /// Require this one-time code at login (two-factor emulation).
    #[arg(long, value_name = "CODE")]
    otp: Option<String>,
    /// Account name accepted at login.
    #[arg(long, default_value = "admin")]
    user: String,
    /// Password accepted at login.
    #[arg(long, default_value = "admin")]
    password: String,
    /// Seed of the synthetic cameras/recordings/events world.
    #[arg(long, default_value_t = 1)]
    seed: u64,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cfg = Config {
        user: cli.user,
        password: cli.password,
        otp: cli.otp,
        fixtures: cli.fixtures.unwrap_or_else(fake_nas::default_fixtures_dir),
        seed: cli.seed,
        advertised_port: cli.port,
    };
    let fixtures = cfg.fixtures.clone();
    let app = match App::new(cfg) {
        Ok(app) => Arc::new(app),
        Err(e) => {
            eprintln!("fake-nas: {e}");
            eprintln!(
                "fake-nas: expected test.h264, test.aac and test.mp4 in {}",
                fixtures.display()
            );
            return ExitCode::FAILURE;
        }
    };

    let listener = match tokio::net::TcpListener::bind((cli.bind.as_str(), cli.port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("fake-nas: cannot listen on {}:{}: {e}", cli.bind, cli.port);
            return ExitCode::FAILURE;
        }
    };
    let addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    println!("fake-nas listening on http://{addr}  (plain HTTP, no TLS)");
    println!(
        "  user: {}  password: {}  OTP: {}",
        app.cfg.user,
        app.cfg.password,
        app.cfg.otp.as_deref().unwrap_or("none")
    );
    println!(
        "  cameras: {} ({} enabled), recordings: last 48 h in 30-min files, fixtures: {}",
        app.world.cameras().len(),
        app.world.enabled_camera_count(),
        fixtures.display()
    );
    println!(
        "  try: SS_PASSWORD={} cargo run -p ss-cli -- --host 127.0.0.1 --port {} --no-https -u {} cameras",
        app.cfg.password, cli.port, app.cfg.user
    );

    match fake_nas::serve(app, listener).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("fake-nas: server error: {e}");
            ExitCode::FAILURE
        }
    }
}
