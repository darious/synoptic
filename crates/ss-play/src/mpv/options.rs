//! mpv option profiles, ported from the reference client's `MpvGLArea`
//! (`ui/mpv_widget.py`: `_on_realize` base options and
//! `_apply_playback_options`) so a later native-widget crate can reuse
//! them unchanged: [`MpvOptions::playback_options`] yields the same
//! `name → value` map the reference writes into libmpv, and
//! [`MpvOptions::to_args`] spells it for the command line.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::sink::Codec;

/// Environment variable naming the audio output driver (`--ao`). Same name
/// as the reference client so an existing setup carries over.
pub const ENV_AO: &str = "SURVEILLANCE_AO";
/// Environment variable with extra mpv options, split like a shell command
/// line: `hwdec=nvdec,vaapi hwdec-extra-frames=12 vd-lavc-fast`. Entries
/// are appended last, so they override the profile.
pub const ENV_MPV_OPTS: &str = "SURVEILLANCE_MPV_OPTS";
/// `ss`-prefixed alias of [`ENV_AO`]; wins when both are set.
pub const ENV_AO_ALIAS: &str = "SS_PLAY_AO";
/// `ss`-prefixed alias of [`ENV_MPV_OPTS`]; wins when both are set.
pub const ENV_MPV_OPTS_ALIAS: &str = "SS_PLAY_MPV_OPTS";

/// Which of the reference's three buffering/timing profiles to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackProfile {
    /// A raw H.264/H.265 pipe from the WebSocket (video only). No cache,
    /// untimed playback off a nominal frame rate, minimum probesize: the
    /// lowest-latency option, used for Live.
    LowLatency,
    /// A live Matroska stream from our own ffmpeg mux (video + audio).
    /// Small cache, timed, short analyzeduration.
    MuxedAudio,
    /// RTSP, HTTP or a local file: a real container with trustworthy
    /// per-frame timing. Small cache, timed.
    Default,
}

/// The cache numbers behind the profiles; the reference exposes these on
/// its Settings page, so they are data rather than constants.
#[derive(Clone, Debug, PartialEq)]
pub struct CacheTuning {
    /// Baseline cache seconds for [`PlaybackProfile::Default`].
    pub cache_seconds_default: f64,
    /// Baseline cache seconds for [`PlaybackProfile::MuxedAudio`].
    pub cache_seconds_muxed_audio: f64,
    /// Baseline cache seconds for [`PlaybackProfile::LowLatency`] (0 = off).
    pub cache_seconds_low_latency: f64,
    /// `demuxer-max-bytes` (MiB) whenever a cache is on.
    pub demuxer_max_bytes_mib: f64,
    /// `demuxer-max-bytes` when no cache runs: the ceiling the demuxer
    /// buffers up to while playback stalls, before back-pressure reaches
    /// the pipe.
    pub demuxer_max_bytes_uncached: String,
    /// `demuxer-max-back-bytes` (MiB); mpv's own 50 MiB default is wasted
    /// on a live stream that never seeks backwards.
    pub demuxer_max_back_bytes_mib: f64,
    /// History speed factor at which extra cache starts being added.
    pub high_speed_cache_enter: f64,
    /// Extra cache seconds added at a 100x history speed.
    pub high_speed_cache_max_seconds: f64,
}

impl Default for CacheTuning {
    fn default() -> Self {
        Self {
            cache_seconds_default: 0.5,
            cache_seconds_muxed_audio: 0.5,
            cache_seconds_low_latency: 0.0,
            demuxer_max_bytes_mib: 32.0,
            demuxer_max_bytes_uncached: "512KiB".into(),
            demuxer_max_back_bytes_mib: 1.0,
            high_speed_cache_enter: 2.0,
            high_speed_cache_max_seconds: 8.0,
        }
    }
}

impl CacheTuning {
    /// Extra cache (seconds) for a History playback `speed`: 0 below
    /// `high_speed_cache_enter`, growing exponentially to
    /// `high_speed_cache_max_seconds` at 100x.
    pub fn high_speed_cache_seconds(&self, speed: f64) -> f64 {
        if speed < self.high_speed_cache_enter {
            return 0.0;
        }
        let growth = self.high_speed_cache_max_seconds.ln() / (100.0 - self.high_speed_cache_enter);
        (growth * (speed - self.high_speed_cache_enter)).exp()
    }

    /// `baseline` plus the high-speed term for `speed`.
    pub fn cache_target_seconds(&self, baseline: f64, speed: f64) -> f64 {
        baseline + self.high_speed_cache_seconds(speed)
    }
}

/// Everything needed to build an mpv command line.
#[derive(Clone, Debug)]
pub struct MpvOptions {
    /// `mpv` executable (name on `PATH` or a path).
    pub binary: PathBuf,
    /// Buffering/timing profile.
    pub profile: PlaybackProfile,
    /// For [`PlaybackProfile::LowLatency`]: tells libavformat the raw
    /// format instead of letting it score 32 probe bytes against every
    /// demuxer it knows. Ignored by the container profiles.
    pub video_format: Option<Codec>,
    /// DSM's History speed multiplier in effect (1.0 for Live); raises the
    /// cache target at high speeds.
    pub history_speed: f64,
    /// Cache numbers.
    pub cache: CacheTuning,
    /// `--hwdec`. `auto-safe` tries the hardware decoders mpv considers
    /// reliable and falls back to software.
    pub hwdec: String,
    /// `--vo`; `None` leaves mpv's default (`gpu`). Tests use `null`.
    pub vo: Option<String>,
    /// `--ao`; `None` leaves mpv's autoprobe. Tests use `null`.
    pub ao: Option<String>,
    /// `--keep-open=yes --idle=yes`: stay up showing the last frame after
    /// EOF (what a window reused across reconnects wants). With `false`
    /// mpv exits when the stream ends.
    pub keep_open: bool,
    /// `--tls-verify` for `https`/`rtsps` sources.
    pub tls_verify: bool,
    /// Force RTSP over TCP (`--demuxer-lavf-o=rtsp_transport=tcp`).
    pub rtsp_tcp: bool,
    /// `--mute`.
    pub mute: bool,
    /// `--volume` (0–100).
    pub volume: u8,
    /// `--start=<seconds>` when non-zero: position inside a recording file.
    pub start_offset: f64,
    /// `--title` for the mpv window.
    pub title: Option<String>,
    /// `--log-file`; the only way to get mpv's diagnostics, since
    /// `--no-terminal` silences stderr (and must, as stdin carries video).
    pub log_file: Option<PathBuf>,
    /// Load the user's `mpv.conf` and scripts. Off by default, matching a
    /// libmpv handle, so playback is reproducible.
    pub load_user_config: bool,
    /// Extra command-line arguments appended last (override everything
    /// above). Populated from the environment by [`MpvOptions::from_env`].
    pub extra_args: Vec<String>,
}

impl MpvOptions {
    /// Base options with a given profile.
    pub fn new(profile: PlaybackProfile) -> Self {
        Self {
            binary: PathBuf::from("mpv"),
            profile,
            video_format: None,
            history_speed: 1.0,
            cache: CacheTuning::default(),
            hwdec: "auto-safe".into(),
            vo: None,
            ao: None,
            keep_open: false,
            tls_verify: true,
            rtsp_tcp: true,
            mute: false,
            volume: 100,
            start_offset: 0.0,
            title: None,
            log_file: None,
            load_user_config: false,
            extra_args: Vec::new(),
        }
    }

    /// Profile for a raw Annex-B pipe of `codec` (Live view).
    pub fn low_latency(codec: Codec) -> Self {
        let mut opts = Self::new(PlaybackProfile::LowLatency);
        opts.video_format = Some(codec);
        opts
    }

    /// Profile for an ffmpeg-muxed Matroska pipe.
    pub fn muxed_audio() -> Self {
        Self::new(PlaybackProfile::MuxedAudio)
    }

    /// Profile for an RTSP/HTTP URL or a local recording.
    pub fn url() -> Self {
        Self::new(PlaybackProfile::Default)
    }

    /// Apply the `SURVEILLANCE_AO` / `SURVEILLANCE_MPV_OPTS` overrides (and
    /// their `SS_PLAY_*` aliases) from the process environment.
    pub fn from_env(mut self) -> Self {
        let ao = std::env::var(ENV_AO_ALIAS)
            .or_else(|_| std::env::var(ENV_AO))
            .unwrap_or_default();
        let ao = ao.trim();
        if !ao.is_empty() {
            tracing::info!("audio output driver from environment: {ao}");
            self.ao = Some(ao.to_string());
        }
        let opts = std::env::var(ENV_MPV_OPTS_ALIAS)
            .or_else(|_| std::env::var(ENV_MPV_OPTS))
            .unwrap_or_default();
        let extra = parse_env_opts(&opts);
        if !extra.is_empty() {
            tracing::info!("extra mpv options from environment: {extra:?}");
            self.extra_args.extend(extra);
        }
        self
    }

    /// Headless settings for tests and smoke checks: `--vo=null --ao=null
    /// --hwdec=no`.
    pub fn headless(mut self) -> Self {
        self.vo = Some("null".into());
        self.ao = Some("null".into());
        self.hwdec = "no".into();
        self
    }

    /// Set the video window title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Write mpv's log to `path`.
    pub fn log_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.log_file = Some(path.into());
        self
    }

    /// Keep the window open after EOF.
    pub fn keep_open(mut self, keep_open: bool) -> Self {
        self.keep_open = keep_open;
        self
    }

    /// The buffering/timing options for the current profile, exactly as
    /// the reference's `_apply_playback_options` writes them (every
    /// profile sets every key). `demuxer-lavf-format` is `""` for the
    /// container profiles, meaning auto-detection.
    pub fn playback_options(&self) -> BTreeMap<&'static str, String> {
        let cache = &self.cache;
        let (baseline, analyzeduration) = match self.profile {
            PlaybackProfile::MuxedAudio => (cache.cache_seconds_muxed_audio, "0.3"),
            PlaybackProfile::LowLatency => (cache.cache_seconds_low_latency, "0"),
            PlaybackProfile::Default => (cache.cache_seconds_default, "0"),
        };
        let target = cache.cache_target_seconds(baseline, self.history_speed);
        let cache_enabled = target > 0.0;
        let low_latency = self.profile == PlaybackProfile::LowLatency;
        let timed = cache_enabled || !low_latency;
        let mut map = BTreeMap::new();
        map.insert("demuxer-lavf-analyzeduration", analyzeduration.to_string());
        map.insert("cache", yes_no(cache_enabled).to_string());
        map.insert(
            "demuxer-max-bytes",
            if cache_enabled {
                format!("{}MiB", trim_float(cache.demuxer_max_bytes_mib))
            } else {
                cache.demuxer_max_bytes_uncached.clone()
            },
        );
        map.insert(
            "demuxer-max-back-bytes",
            format!("{}MiB", trim_float(cache.demuxer_max_back_bytes_mib)),
        );
        map.insert("demuxer-readahead-secs", trim_float(target));
        map.insert("cache-secs", trim_float(target));
        map.insert("correct-pts", yes_no(timed).to_string());
        map.insert("untimed", yes_no(!timed).to_string());
        map.insert(
            "container-fps-override",
            if timed { "0" } else { "25" }.to_string(),
        );
        map.insert(
            "demuxer-lavf-probesize",
            if timed { "32768" } else { "32" }.to_string(),
        );
        map.insert(
            "demuxer-lavf-format",
            if low_latency {
                self.video_format
                    .map(Codec::ffmpeg_format)
                    .unwrap_or("")
                    .to_string()
            } else {
                String::new()
            },
        );
        map
    }

    /// The complete argument list (without the source). `ipc_socket`, when
    /// given, adds `--input-ipc-server=<path>`.
    pub fn to_args(&self, ipc_socket: Option<&std::path::Path>) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        if !self.load_user_config {
            args.push("--no-config".into());
        }
        // stdin may carry the video stream: nothing may touch the terminal.
        args.push("--no-terminal".into());
        args.push(format!("--hwdec={}", self.hwdec));
        if let Some(vo) = &self.vo {
            args.push(format!("--vo={vo}"));
        }
        if let Some(ao) = &self.ao {
            args.push(format!("--ao={ao}"));
        }
        args.push(format!("--keep-open={}", yes_no(self.keep_open)));
        args.push(format!("--idle={}", yes_no(self.keep_open)));
        args.push("--input-default-bindings=no".into());
        args.push("--input-vo-keyboard=no".into());
        args.push("--osc=no".into());
        if self.rtsp_tcp {
            args.push("--demuxer-lavf-o=rtsp_transport=tcp".into());
        }
        args.push(format!("--tls-verify={}", yes_no(self.tls_verify)));
        args.push(format!("--mute={}", yes_no(self.mute)));
        args.push(format!("--volume={}", self.volume));
        if self.start_offset > 0.0 {
            args.push(format!("--start={}", self.start_offset));
        }
        if let Some(title) = &self.title {
            args.push(format!("--title={title}"));
        }
        if let Some(log) = &self.log_file {
            args.push(format!("--log-file={}", log.display()));
        }
        for (name, value) in self.playback_options() {
            if name == "demuxer-lavf-format" && value.is_empty() {
                continue; // auto-detect: mpv's default, nothing to pass
            }
            args.push(format!("--{name}={value}"));
        }
        if let Some(sock) = ipc_socket {
            args.push(format!("--input-ipc-server={}", sock.display()));
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// `0.5` → `"0.5"`, `32.0` → `"32"` (Python's `%g`-ish, enough for mpv).
fn trim_float(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Turn `SURVEILLANCE_MPV_OPTS` into command-line arguments: split like a
/// shell (whitespace separates, single/double quotes group, backslash
/// escapes), then `name=value` → `--name=value`, bare `name` → `--name`.
/// Commas survive (`hwdec=nvdec,vaapi` is mpv list syntax). Nothing is
/// validated: mpv refuses an unknown option at startup, and the sink
/// reports that exit.
pub fn parse_env_opts(spec: &str) -> Vec<String> {
    shell_split(spec)
        .into_iter()
        .filter(|item| !item.is_empty())
        .map(|item| {
            if item.starts_with("--") {
                item
            } else {
                format!("--{item}")
            }
        })
        .collect()
}

/// Minimal POSIX-ish word splitting (quotes and backslashes; no expansion).
pub(crate) fn shell_split(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    cur.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => {
                            if let Some(&n) = chars.peek()
                                && matches!(n, '"' | '\\' | '$' | '`')
                            {
                                cur.push(n);
                                chars.next();
                            } else {
                                cur.push('\\');
                            }
                        }
                        _ => cur.push(q),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn applied(profile: PlaybackProfile, speed: f64) -> BTreeMap<&'static str, String> {
        let mut opts = MpvOptions::new(profile);
        opts.history_speed = speed;
        opts.playback_options()
    }

    #[test]
    fn every_profile_writes_the_same_options() {
        let low: Vec<_> = applied(PlaybackProfile::LowLatency, 1.0)
            .into_keys()
            .collect();
        let muxed: Vec<_> = applied(PlaybackProfile::MuxedAudio, 1.0)
            .into_keys()
            .collect();
        let default: Vec<_> = applied(PlaybackProfile::Default, 1.0).into_keys().collect();
        assert_eq!(low, muxed);
        assert_eq!(low, default);
        assert_eq!(low.len(), 11);
    }

    #[test]
    fn low_latency_cache_stays_off_in_live() {
        let live = applied(PlaybackProfile::LowLatency, 1.0);
        assert_eq!(live["cache"], "no");
        assert_eq!(live["cache-secs"], "0");
        assert_eq!(live["demuxer-max-bytes"], "512KiB");
        assert_eq!(live["correct-pts"], "no");
        assert_eq!(live["untimed"], "yes");
        assert_eq!(live["container-fps-override"], "25");
        assert_eq!(live["demuxer-lavf-probesize"], "32");
        assert_eq!(live["demuxer-lavf-analyzeduration"], "0");
    }

    #[test]
    fn low_latency_cache_turns_on_at_high_history_speed() {
        let history = applied(PlaybackProfile::LowLatency, 10.0);
        assert_eq!(history["cache"], "yes");
        assert!(history["cache-secs"].parse::<f64>().unwrap() > 0.0);
        assert_eq!(history["demuxer-max-bytes"], "32MiB");
        // once it buffers there is real timing to keep
        assert_eq!(history["untimed"], "no");
        assert_eq!(history["correct-pts"], "yes");
        assert_eq!(history["demuxer-lavf-probesize"], "32768");
    }

    #[test]
    fn only_low_latency_is_ever_untimed() {
        for profile in [PlaybackProfile::MuxedAudio, PlaybackProfile::Default] {
            let o = applied(profile, 1.0);
            assert_eq!(o["correct-pts"], "yes");
            assert_eq!(o["untimed"], "no");
            assert_eq!(o["container-fps-override"], "0");
            assert_eq!(o["demuxer-lavf-probesize"], "32768");
            assert_eq!(o["cache"], "yes");
            assert_eq!(o["cache-secs"], "0.5");
            assert_eq!(o["demuxer-max-back-bytes"], "1MiB");
            assert_eq!(o["demuxer-lavf-format"], "");
        }
        assert_eq!(
            applied(PlaybackProfile::MuxedAudio, 1.0)["demuxer-lavf-analyzeduration"],
            "0.3"
        );
    }

    #[test]
    fn a_zero_cache_does_not_make_a_container_stream_raw() {
        for profile in [PlaybackProfile::MuxedAudio, PlaybackProfile::Default] {
            let mut opts = MpvOptions::new(profile);
            opts.cache.cache_seconds_default = 0.0;
            opts.cache.cache_seconds_muxed_audio = 0.0;
            let o = opts.playback_options();
            assert_eq!(o["cache"], "no");
            assert_eq!(o["untimed"], "no");
            assert_eq!(o["correct-pts"], "yes");
            assert_eq!(o["demuxer-lavf-probesize"], "32768");
        }
    }

    #[test]
    fn high_history_speed_raises_the_default_profiles_cache() {
        let live = applied(PlaybackProfile::Default, 1.0);
        let history = applied(PlaybackProfile::Default, 100.0);
        let live_secs: f64 = live["cache-secs"].parse().unwrap();
        let history_secs: f64 = history["cache-secs"].parse().unwrap();
        assert_eq!(live_secs, 0.5);
        assert!((history_secs - 8.5).abs() < 1e-6, "{history_secs}");
    }

    #[test]
    fn high_speed_cache_curve() {
        let t = CacheTuning::default();
        assert_eq!(t.high_speed_cache_seconds(1.0), 0.0);
        assert_eq!(t.high_speed_cache_seconds(1.99), 0.0);
        assert!((t.high_speed_cache_seconds(2.0) - 1.0).abs() < 1e-9);
        assert!((t.high_speed_cache_seconds(100.0) - 8.0).abs() < 1e-9);
        let (a, b, c) = (
            t.high_speed_cache_seconds(5.0),
            t.high_speed_cache_seconds(20.0),
            t.high_speed_cache_seconds(50.0),
        );
        assert!(0.0 < a && a < b && b < c && c < 8.0);
    }

    #[test]
    fn low_latency_is_told_the_format() {
        let h264 = MpvOptions::low_latency(Codec::H264).playback_options();
        assert_eq!(h264["demuxer-lavf-format"], "h264");
        let hevc = MpvOptions::low_latency(Codec::H265).playback_options();
        assert_eq!(hevc["demuxer-lavf-format"], "hevc");
        let args = MpvOptions::low_latency(Codec::H265).to_args(None);
        assert!(args.contains(&"--demuxer-lavf-format=hevc".to_string()));
        assert!(args.contains(&"--untimed=yes".to_string()));
        assert!(args.contains(&"--no-terminal".to_string()));
        assert!(args.contains(&"--hwdec=auto-safe".to_string()));
        assert!(args.contains(&"--no-config".to_string()));
        let url_args = MpvOptions::url().to_args(None);
        assert!(
            !url_args
                .iter()
                .any(|a| a.starts_with("--demuxer-lavf-format"))
        );
        assert!(url_args.contains(&"--demuxer-lavf-o=rtsp_transport=tcp".to_string()));
        assert!(url_args.contains(&"--tls-verify=yes".to_string()));
    }

    #[test]
    fn start_offset_is_only_passed_when_set() {
        let mut opts = MpvOptions::url();
        assert!(!opts.to_args(None).iter().any(|a| a.starts_with("--start=")));
        opts.start_offset = 42.5;
        assert!(opts.to_args(None).contains(&"--start=42.5".to_string()));
    }

    #[test]
    fn ipc_socket_and_extras_come_last() {
        let mut opts = MpvOptions::url();
        opts.extra_args = vec!["--hwdec=nvdec".into()];
        let args = opts.to_args(Some(std::path::Path::new("/run/x.sock")));
        let n = args.len();
        assert_eq!(args[n - 2], "--input-ipc-server=/run/x.sock");
        assert_eq!(args[n - 1], "--hwdec=nvdec");
        // the profile's own hwdec is still there, earlier: mpv takes the last one
        assert!(args.iter().position(|a| a == "--hwdec=auto-safe").unwrap() < n - 1);
    }

    #[test]
    fn env_opts_unset_means_nothing() {
        assert!(parse_env_opts("").is_empty());
        assert!(parse_env_opts("   ").is_empty());
    }

    #[test]
    fn env_opts_split_on_whitespace_and_keep_commas() {
        assert_eq!(
            parse_env_opts("hwdec=nvdec,vaapi  hwdec-extra-frames=12"),
            vec!["--hwdec=nvdec,vaapi", "--hwdec-extra-frames=12"]
        );
    }

    #[test]
    fn env_opts_bare_entry_is_a_flag_whatever_it_looks_like() {
        assert_eq!(
            parse_env_opts("vd-lavc-fast hwdec-extra-frames:12"),
            vec!["--vd-lavc-fast", "--hwdec-extra-frames:12"]
        );
    }

    #[test]
    fn env_opts_value_keeps_everything_after_the_first_equals() {
        assert_eq!(
            parse_env_opts("vd-lavc-o=strict=1"),
            vec!["--vd-lavc-o=strict=1"]
        );
    }

    #[test]
    fn env_opts_shell_quoting_protects_whitespace() {
        assert_eq!(
            parse_env_opts("title='Front door' hwdec=no \"vo=gpu-next\" a\\ b"),
            vec!["--title=Front door", "--hwdec=no", "--vo=gpu-next", "--a b"]
        );
        assert_eq!(parse_env_opts("--already-dashed"), vec!["--already-dashed"]);
    }
}
