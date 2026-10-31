//! `ss`: a headless command-line client for manual integration testing of
//! the `ss-api`, `ss-stream`, `ss-play` and `ss-config` crates against a
//! real Surveillance Station.
//!
//! The binary lives in `main.rs`; everything it does is in this library so
//! the integration tests can drive the same code paths (argument parsing,
//! time-window maths, the frame→sink pump) without a NAS.
//!
//! | module | what |
//! |---|---|
//! | [`cli`] | the `clap` command tree |
//! | [`session`] | profile resolution, credentials, login/logout |
//! | [`pump`] | MixStream [`ss_stream::Event`]s → [`ss_play::FrameSink`], with stats |
//! | [`sink`] | `--sink mpv|raw|mp4` → a boxed sink |
//! | [`select`] | time window and "which recording covers this instant" |
//! | [`timefmt`] | local-time parsing and formatting |
//! | [`output`] | tables, byte/duration formatting, secret redaction |
//! | [`commands`] | one function per subcommand |
//!
//! Nothing here prints or logs a session id, a password or an OTP code;
//! the tracing output additionally passes through [`output::redact`].

pub mod cli;
pub mod commands;
pub mod output;
pub mod pump;
pub mod select;
pub mod session;
pub mod sink;
pub mod timefmt;

use std::fmt;

/// An error that asks `main` for a specific exit status (anything else
/// exits 1). Code 3 is "two-factor authentication required".
#[derive(Debug)]
pub struct CliExit {
    /// Process exit status.
    pub code: u8,
    /// What to print on stderr.
    pub message: String,
}

impl CliExit {
    /// Exit code for [`ss_api::Error::OtpRequired`].
    pub const OTP_REQUIRED: u8 = 3;

    /// A new exit request.
    pub fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for CliExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliExit {}

/// Run the parsed command line.
pub async fn run(cli: cli::Cli) -> anyhow::Result<()> {
    commands::dispatch(cli).await
}
