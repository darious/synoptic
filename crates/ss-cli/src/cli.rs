//! The command tree. `--profile`, `--host`, `--port`, `--https/--no-https`,
//! `--insecure` and `--user` are global so they can go before or after the
//! subcommand; every subcommand but `login` falls back to the saved profile
//! for whatever is not given.
//!
//! The Surveillance Station stream *quality* (high/balanced/low) is
//! `--quality` (alias `--stream-profile`): `--profile` is taken by the
//! connection profile name and a flag cannot mean two things.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

/// `ss`: Surveillance Station command-line test client.
#[derive(Parser, Debug)]
#[command(
    name = "ss",
    version,
    about = "Surveillance Station command-line test client",
    long_about = "Logs in to a Synology Surveillance Station, lists cameras and \
                  recordings, and plays or records live and historical video \
                  through mpv/ffmpeg. Built for manual integration testing of \
                  the ss-* crates against a real NAS."
)]
pub struct Cli {
    /// More logging on stderr: -v debug, -vv trace (RUST_LOG overrides).
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Connection profile to use (default: the config's default profile;
    /// for `login`: the name to save under, default = host).
    #[arg(long, global = true, value_name = "NAME")]
    pub profile: Option<String>,

    #[command(flatten)]
    pub connect: ConnectArgs,

    #[command(subcommand)]
    pub command: Command,
}

/// How to reach the NAS. Each field overrides the profile's value.
#[derive(Args, Debug, Clone, Default)]
pub struct ConnectArgs {
    /// NAS host name or IP.
    #[arg(long, global = true, value_name = "HOST")]
    pub host: Option<String>,

    /// DSM port (default 5001 for https, 5000 for http).
    #[arg(long, global = true, value_name = "PORT")]
    pub port: Option<u16>,

    /// Use HTTPS (the default).
    #[arg(long, global = true, overrides_with = "no_https")]
    pub https: bool,

    /// Use plain HTTP.
    #[arg(long, global = true, overrides_with = "https")]
    pub no_https: bool,

    /// Accept a self-signed / invalid TLS certificate.
    #[arg(long, global = true)]
    pub insecure: bool,

    /// DSM account name.
    #[arg(short, long, global = true, value_name = "USER")]
    pub user: Option<String>,
}

impl ConnectArgs {
    /// `Some(true)` for `--https`, `Some(false)` for `--no-https`, `None`
    /// when neither was given (use the profile's or the default).
    pub fn https(&self) -> Option<bool> {
        if self.no_https {
            Some(false)
        } else if self.https {
            Some(true)
        } else {
            None
        }
    }
}

/// The subcommands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Log in, print the Surveillance Station version and camera count,
    /// save the profile and store the credentials in the keyring.
    Login(LoginArgs),
    /// List the cameras.
    Cameras,
    /// Print a camera's GetLiveViewPath fields and its MixStream WebSocket URL.
    Paths(PathsArgs),
    /// Play or record a camera's live MixStream.
    Live(LiveArgs),
    /// Play or record a recording from a point in time.
    History(HistoryArgs),
    /// List recordings.
    Recordings(RecordingsArgs),
    /// Download a recording to a file.
    Download(DownloadArgs),
    /// Play a camera's RTSP (or RTSP-over-HTTP / multicast / MJPEG) URL in mpv.
    Rtsp(RtspArgs),
}

/// `ss login`.
#[derive(Args, Debug, Clone, Default)]
pub struct LoginArgs {
    /// Read the password from the first line of standard input
    /// (else `$SS_PASSWORD`, else an interactive prompt).
    #[arg(long)]
    pub password_stdin: bool,

    /// Two-factor authentication code.
    #[arg(long, value_name = "CODE")]
    pub otp: Option<String>,

    /// Ask DSM for a trusted-device token so later logins skip the OTP.
    #[arg(long)]
    pub trust_device: bool,

    /// Make this profile the default (the first profile saved always is).
    #[arg(long)]
    pub set_default: bool,

    /// Do not store the credentials in the system keyring.
    #[arg(long)]
    pub no_keyring: bool,
}

/// Surveillance Station stream quality (`profile=` on the MixStream URL).
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    High,
    Balanced,
    Low,
}

impl From<Quality> for ss_stream::StreamProfile {
    fn from(q: Quality) -> Self {
        match q {
            Quality::High => Self::High,
            Quality::Balanced => Self::Balanced,
            Quality::Low => Self::Low,
        }
    }
}

/// Video codec override for `live`/`history`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecArg {
    H264,
    H265,
}

impl From<CodecArg> for ss_play::Codec {
    fn from(c: CodecArg) -> Self {
        match c {
            CodecArg::H264 => Self::H264,
            CodecArg::H265 => Self::H265,
        }
    }
}

/// Where the video goes.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SinkKind {
    /// An mpv window fed over a pipe.
    #[default]
    Mpv,
    /// The elementary stream as-is (`.h264` / `.h265`).
    Raw,
    /// Remuxed by ffmpeg into `--out` (`.mp4`, `.mkv`, … by extension).
    Mp4,
}

/// Sink and stop conditions shared by `live` and `history`.
#[derive(Args, Debug, Clone, Default)]
pub struct PlayArgs {
    /// Where the video goes.
    #[arg(long, value_enum, default_value_t = SinkKind::Mpv)]
    pub sink: SinkKind,

    /// Output file for `--sink raw|mp4` (default: `ss-cam<ID>-<unix>.<ext>`).
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,

    /// Stop after this many seconds (default: run until Ctrl-C).
    #[arg(long, value_name = "N")]
    pub seconds: Option<f64>,

    /// Force the video codec instead of trusting DSM's codec-info frame.
    #[arg(long, value_enum)]
    pub codec: Option<CodecArg>,
}

/// `ss paths`.
#[derive(Args, Debug, Clone)]
pub struct PathsArgs {
    /// Camera id.
    pub camera_id: i64,

    /// Stream quality for the WebSocket URL (default: the camera's Live
    /// View setting, Balanced if the NAS does not say).
    #[arg(long, visible_alias = "stream-profile", value_enum)]
    pub quality: Option<Quality>,
}

/// `ss live`.
#[derive(Args, Debug, Clone)]
pub struct LiveArgs {
    /// Camera id.
    pub camera_id: i64,

    /// Stream quality (default: the camera's Live View setting).
    #[arg(long, visible_alias = "stream-profile", value_enum)]
    pub quality: Option<Quality>,

    #[command(flatten)]
    pub play: PlayArgs,
}

/// `ss history`.
#[derive(Args, Debug, Clone)]
pub struct HistoryArgs {
    /// Camera id.
    pub camera_id: i64,

    /// Instant to play from, local time: "YYYY-MM-DD HH:MM:SS" (seconds
    /// optional, 'T' separator accepted) or unix seconds.
    #[arg(long, value_name = "TIME")]
    pub at: String,

    /// Playback speed multiplier (0.5, 1, 2, 4, …).
    #[arg(long, default_value_t = 1.0)]
    pub speed: f64,

    /// Play backwards.
    #[arg(long)]
    pub reverse: bool,

    /// Half-width in seconds of the Recording.List window around --at.
    #[arg(long, default_value_t = 4 * 3600, value_name = "SECS")]
    pub window: i64,

    #[command(flatten)]
    pub play: PlayArgs,
}

/// `ss recordings`.
#[derive(Args, Debug, Clone, Default)]
pub struct RecordingsArgs {
    /// Only this camera.
    #[arg(long, value_name = "ID")]
    pub camera: Option<i64>,

    /// Earliest start, local time or unix seconds.
    #[arg(long, value_name = "TIME")]
    pub from: Option<String>,

    /// Latest start, local time or unix seconds.
    #[arg(long, value_name = "TIME")]
    pub to: Option<String>,

    /// Page size.
    #[arg(long, default_value_t = 20)]
    pub limit: u32,

    /// Page offset.
    #[arg(long, default_value_t = 0)]
    pub offset: u32,
}

/// `ss download`.
#[derive(Args, Debug, Clone)]
pub struct DownloadArgs {
    /// Recording id (see `ss recordings`).
    pub recording_id: i64,

    /// Destination file.
    #[arg(long, value_name = "FILE")]
    pub out: PathBuf,
}

/// Live view transport for `ss rtsp`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    #[default]
    Rtsp,
    RtspOverHttp,
    Multicast,
    Mjpeg,
}

impl From<Protocol> for ss_api::LiveProtocol {
    fn from(p: Protocol) -> Self {
        match p {
            Protocol::Rtsp => Self::Rtsp,
            Protocol::RtspOverHttp => Self::RtspOverHttp,
            Protocol::Multicast => Self::Multicast,
            Protocol::Mjpeg => Self::Mjpeg,
        }
    }
}

/// `ss rtsp`.
#[derive(Args, Debug, Clone)]
pub struct RtspArgs {
    /// Camera id.
    pub camera_id: i64,

    /// Which GetLiveViewPath entry to play.
    #[arg(long, value_enum, default_value_t = Protocol::Rtsp)]
    pub protocol: Protocol,

    /// Quit mpv after this many seconds (default: until the window closes or Ctrl-C).
    #[arg(long, value_name = "N")]
    pub seconds: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("ss").chain(args.iter().copied()))
            .unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    #[test]
    fn command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn login_flags() {
        let cli = parse(&[
            "login",
            "--host",
            "nas",
            "--port",
            "5001",
            "--insecure",
            "--user",
            "admin",
            "--profile",
            "home",
            "--otp",
            "123456",
            "--trust-device",
            "--password-stdin",
        ]);
        assert_eq!(cli.connect.host.as_deref(), Some("nas"));
        assert_eq!(cli.connect.port, Some(5001));
        assert!(cli.connect.insecure);
        assert_eq!(cli.connect.user.as_deref(), Some("admin"));
        assert_eq!(cli.profile.as_deref(), Some("home"));
        assert_eq!(cli.connect.https(), None);
        match cli.command {
            Command::Login(l) => {
                assert_eq!(l.otp.as_deref(), Some("123456"));
                assert!(l.trust_device && l.password_stdin);
                assert!(!l.set_default && !l.no_keyring);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn https_flags_resolve_and_last_one_wins() {
        assert_eq!(parse(&["cameras"]).connect.https(), None);
        assert_eq!(parse(&["--https", "cameras"]).connect.https(), Some(true));
        assert_eq!(
            parse(&["--no-https", "cameras"]).connect.https(),
            Some(false)
        );
        assert_eq!(
            parse(&["--https", "--no-https", "cameras"]).connect.https(),
            Some(false)
        );
        assert_eq!(
            parse(&["--no-https", "--https", "cameras"]).connect.https(),
            Some(true)
        );
    }

    #[test]
    fn globals_may_follow_the_subcommand() {
        let cli = parse(&["cameras", "--host", "10.0.0.2", "--no-https", "-vv"]);
        assert_eq!(cli.connect.host.as_deref(), Some("10.0.0.2"));
        assert_eq!(cli.connect.https(), Some(false));
        assert_eq!(cli.verbose, 2);
        assert!(matches!(cli.command, Command::Cameras));
    }

    #[test]
    fn live_defaults_and_options() {
        let cli = parse(&["live", "7"]);
        let Command::Live(l) = cli.command else {
            panic!()
        };
        assert_eq!(l.camera_id, 7);
        assert_eq!(l.quality, None);
        assert_eq!(l.play.sink, SinkKind::Mpv);
        assert_eq!(l.play.out, None);
        assert_eq!(l.play.seconds, None);
        assert_eq!(l.play.codec, None);

        let cli = parse(&[
            "live",
            "7",
            "--quality",
            "low",
            "--sink",
            "mp4",
            "--out",
            "x.mp4",
            "--seconds",
            "10",
            "--codec",
            "h265",
        ]);
        let Command::Live(l) = cli.command else {
            panic!()
        };
        assert_eq!(l.quality, Some(Quality::Low));
        assert_eq!(l.play.sink, SinkKind::Mp4);
        assert_eq!(l.play.out.as_deref(), Some(std::path::Path::new("x.mp4")));
        assert_eq!(l.play.seconds, Some(10.0));
        assert_eq!(l.play.codec, Some(CodecArg::H265));

        // The alias from the spec works too.
        let cli = parse(&["live", "7", "--stream-profile", "high"]);
        let Command::Live(l) = cli.command else {
            panic!()
        };
        assert_eq!(l.quality, Some(Quality::High));
    }

    #[test]
    fn history_flags() {
        let cli = parse(&[
            "history",
            "3",
            "--at",
            "2026-10-30 21:15:00",
            "--seconds",
            "30",
            "--speed",
            "4",
            "--reverse",
            "--sink",
            "raw",
        ]);
        let Command::History(h) = cli.command else {
            panic!()
        };
        assert_eq!(h.camera_id, 3);
        assert_eq!(h.at, "2026-10-30 21:15:00");
        assert_eq!(h.speed, 4.0);
        assert!(h.reverse);
        assert_eq!(h.window, 4 * 3600);
        assert_eq!(h.play.sink, SinkKind::Raw);
        assert_eq!(h.play.seconds, Some(30.0));
        assert!(
            Cli::try_parse_from(["ss", "history", "3"]).is_err(),
            "--at is required"
        );
    }

    #[test]
    fn recordings_download_rtsp() {
        let cli = parse(&[
            "recordings",
            "--camera",
            "2",
            "--from",
            "2026-10-01 00:00",
            "--limit",
            "5",
        ]);
        let Command::Recordings(r) = cli.command else {
            panic!()
        };
        assert_eq!(r.camera, Some(2));
        assert_eq!(r.from.as_deref(), Some("2026-10-01 00:00"));
        assert_eq!(r.to, None);
        assert_eq!((r.limit, r.offset), (5, 0));
        assert_eq!(parse(&["recordings"]).connect.host, None);

        let cli = parse(&["download", "42", "--out", "rec.mp4"]);
        let Command::Download(d) = cli.command else {
            panic!()
        };
        assert_eq!(d.recording_id, 42);
        assert_eq!(d.out, PathBuf::from("rec.mp4"));

        let cli = parse(&["rtsp", "5", "--protocol", "rtsp-over-http"]);
        let Command::Rtsp(r) = cli.command else {
            panic!()
        };
        assert_eq!(r.protocol, Protocol::RtspOverHttp);
        assert_eq!(
            ss_api::LiveProtocol::from(r.protocol),
            ss_api::LiveProtocol::RtspOverHttp
        );
        assert_eq!(parse(&["rtsp", "5"]).connect.port, None);
    }

    #[test]
    fn enum_conversions() {
        assert_eq!(ss_stream::StreamProfile::from(Quality::High).number(), 0);
        assert_eq!(ss_stream::StreamProfile::from(Quality::Low).number(), 2);
        assert_eq!(ss_play::Codec::from(CodecArg::H264), ss_play::Codec::H264);
        assert_eq!(ss_play::Codec::from(CodecArg::H265), ss_play::Codec::H265);
    }
}
