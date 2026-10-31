use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;
use ss_cli::CliExit;
use ss_cli::cli::Cli;
use ss_cli::output::RedactingStderr;
use tracing_subscriber::EnvFilter;

/// `RUST_LOG` wins; else `-v` raises our own crates to debug, `-vv` to
/// trace, everything else stays at warn so no HTTP library can echo a URL.
fn init_tracing(verbose: u8) {
    let filter = match EnvFilter::try_from_default_env() {
        Ok(f) => f,
        Err(_) => {
            let level = match verbose {
                0 => "info",
                1 => "debug",
                _ => "trace",
            };
            let spec = format!(
                "warn,ss_cli={level},ss_api={level},ss_stream={level},ss_play={level},ss_config={level}"
            );
            EnvFilter::new(spec)
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(RedactingStderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_target(verbose > 0)
        .without_time()
        .init();
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    match ss_cli::run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if let Some(exit) = e.downcast_ref::<CliExit>() {
                eprintln!("error: {exit}");
                return ExitCode::from(exit.code);
            }
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
