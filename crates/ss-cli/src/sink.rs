//! `--sink mpv|raw|mp4 [--out FILE]` → a boxed [`FrameSink`], opened once
//! the stream's codec is known (mpv and the raw file name need it).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use ss_play::{
    Codec, FfmpegRemuxOptions, FfmpegRemuxSink, FrameSink, MpvOptions, MpvPipeSink, RawFileSink,
    Timestamps,
};

use crate::cli::{PlayArgs, SinkKind};

/// What `live`/`history` should feed the video into.
#[derive(Debug, Clone, PartialEq)]
pub enum SinkSpec {
    /// An mpv window fed over a pipe (`MpvPipeSink`, low-latency profile).
    Mpv {
        /// Window title.
        title: String,
        /// History playback speed, for mpv's cache sizing (1.0 for live).
        history_speed: f64,
    },
    /// The Annex-B elementary stream written as-is.
    Raw {
        /// Output path; `None` derives `<default_stem>.h264|h265`.
        path: Option<PathBuf>,
        /// Stem for the default file name.
        default_stem: String,
    },
    /// ffmpeg `-c copy` remux into a container chosen by extension.
    Mp4 {
        /// Output path; `None` derives `<default_stem>.mp4`.
        path: Option<PathBuf>,
        /// Stem for the default file name.
        default_stem: String,
        /// Timestamp strategy.
        timestamps: Timestamps,
    },
}

impl SinkSpec {
    /// Build from the command-line flags. `label` names the stream for the
    /// mpv title and the default file stem (`cam7`, `cam7-history`).
    pub fn from_args(args: &PlayArgs, label: &str, history_speed: f64) -> Self {
        let stem = format!("ss-{label}-{}", unix_now());
        match args.sink {
            SinkKind::Mpv => SinkSpec::Mpv {
                title: format!("ss {label}"),
                history_speed,
            },
            SinkKind::Raw => SinkSpec::Raw {
                path: args.out.clone(),
                default_stem: stem,
            },
            SinkKind::Mp4 => SinkSpec::Mp4 {
                path: args.out.clone(),
                default_stem: stem,
                timestamps: Timestamps::Wallclock,
            },
        }
    }

    /// The file this sink writes, once `codec` is known (`None` for mpv).
    pub fn output_path(&self, codec: Codec) -> Option<PathBuf> {
        match self {
            SinkSpec::Mpv { .. } => None,
            SinkSpec::Raw { path, default_stem } => Some(path.clone().unwrap_or_else(|| {
                PathBuf::from(format!("{default_stem}.{}", codec.file_extension()))
            })),
            SinkSpec::Mp4 {
                path, default_stem, ..
            } => Some(
                path.clone()
                    .unwrap_or_else(|| PathBuf::from(format!("{default_stem}.mp4"))),
            ),
        }
    }

    /// Open the sink for a stream of `codec`.
    pub async fn open(&self, codec: Codec) -> Result<Box<dyn FrameSink>> {
        match self {
            SinkSpec::Mpv {
                title,
                history_speed,
            } => {
                let mut options = MpvOptions::low_latency(codec)
                    .from_env()
                    .title(title.clone());
                options.history_speed = *history_speed;
                let sink = MpvPipeSink::spawn_with(options, codec).context("starting mpv")?;
                Ok(Box::new(sink))
            }
            SinkSpec::Raw { .. } => {
                let path = self.output_path(codec).expect("raw sink has a path");
                let sink = RawFileSink::create(&path)
                    .await
                    .with_context(|| format!("creating {}", path.display()))?;
                Ok(Box::new(sink))
            }
            SinkSpec::Mp4 { timestamps, .. } => {
                let path = self.output_path(codec).expect("mp4 sink has a path");
                let options = FfmpegRemuxOptions::new(&path).timestamps(*timestamps);
                let mut sink = FfmpegRemuxSink::with_options(options);
                sink.start(codec)
                    .await
                    .with_context(|| format!("starting ffmpeg for {}", path.display()))?;
                Ok(Box::new(sink))
            }
        }
    }

    /// Whether this sink shows video on screen (so a headless run needs `--sink raw|mp4`).
    pub fn is_display(&self) -> bool {
        matches!(self, SinkSpec::Mpv { .. })
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A raw-file sink spec for an explicit path (tests, scripting).
pub fn raw_to(path: impl AsRef<Path>) -> SinkSpec {
    SinkSpec::Raw {
        path: Some(path.as_ref().to_path_buf()),
        default_stem: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_names_follow_the_codec() {
        let args = PlayArgs {
            sink: SinkKind::Raw,
            ..Default::default()
        };
        let spec = SinkSpec::from_args(&args, "cam7", 1.0);
        let h264 = spec.output_path(Codec::H264).unwrap();
        let h265 = spec.output_path(Codec::H265).unwrap();
        assert!(h264.to_string_lossy().starts_with("ss-cam7-"));
        assert!(h264.extension().is_some_and(|e| e == "h264"));
        assert!(h265.extension().is_some_and(|e| e == "h265"));

        let args = PlayArgs {
            sink: SinkKind::Mp4,
            out: Some(PathBuf::from("/tmp/x.mkv")),
            ..Default::default()
        };
        let spec = SinkSpec::from_args(&args, "cam7", 1.0);
        assert_eq!(
            spec.output_path(Codec::H264),
            Some(PathBuf::from("/tmp/x.mkv"))
        );
        assert!(!spec.is_display());

        let spec = SinkSpec::from_args(&PlayArgs::default(), "cam7", 4.0);
        assert!(spec.is_display());
        assert_eq!(spec.output_path(Codec::H264), None);
        assert!(matches!(spec, SinkSpec::Mpv { history_speed, .. } if history_speed == 4.0));
        assert_eq!(
            raw_to("a.h264").output_path(Codec::H265),
            Some(PathBuf::from("a.h264"))
        );
    }
}
