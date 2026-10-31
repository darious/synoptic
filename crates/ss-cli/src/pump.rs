//! The frame pump: MixStream [`Event`]s in, [`AccessUnit`]s out to a
//! [`FrameSink`], with a stats line every couple of seconds.
//!
//! `live` and `history` both run [`run`]; the integration tests feed it
//! from a plain channel instead of a WebSocket.

use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ss_play::{AccessUnit, Codec, FrameSink};
use ss_stream::{Event, FrameKind, VideoCodec};
use tokio::sync::mpsc;

use crate::output::format_bytes;
use crate::sink::SinkSpec;

/// Knobs for [`run`].
#[derive(Debug, Clone)]
pub struct PumpOptions {
    /// Trust this codec instead of DSM's codec-info frame.
    pub codec_override: Option<Codec>,
    /// How often a stats line goes to stderr (`None` = never).
    pub stats_interval: Option<Duration>,
    /// Stop once a video frame's `msec` reaches this (forward history
    /// playback: the recording's end).
    pub stop_at_ms: Option<u64>,
    /// Prefix for the stats line (`live`, `history`).
    pub label: String,
}

impl Default for PumpOptions {
    fn default() -> Self {
        Self {
            codec_override: None,
            stats_interval: Some(Duration::from_secs(2)),
            stop_at_ms: None,
            label: "stream".into(),
        }
    }
}

/// Counters, final and running.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PumpStats {
    /// Video access units pushed to the sink.
    pub frames: u64,
    /// Bytes pushed (start codes included).
    pub bytes: u64,
    /// Audio frames seen (counted, not played).
    pub audio_frames: u64,
    /// Reconnects (`Connected` events beyond the first).
    pub reconnects: u32,
    /// Disconnects reported.
    pub disconnects: u32,
    /// `msec` of the last video frame, when DSM sent one.
    pub last_msec: Option<u64>,
    /// Codec the sink was opened for.
    pub codec: Option<Codec>,
    /// `Event::GaveUp` reason, if the stream was lost.
    pub gave_up: Option<String>,
    /// Why the pump stopped.
    pub stopped_by: Stop,
}

/// Why [`run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stop {
    /// The caller's stop future resolved (Ctrl-C, `--seconds`).
    #[default]
    Requested,
    /// The event channel ended (`Event::Stopped` or sender dropped).
    StreamEnded,
    /// `Event::GaveUp`.
    GaveUp,
    /// `stop_at_ms` reached.
    EndReached,
}

fn map_codec(v: &VideoCodec) -> Option<Codec> {
    match v {
        VideoCodec::H264 => Some(Codec::H264),
        VideoCodec::H265 => Some(Codec::H265),
        VideoCodec::Other(name) => Codec::from_name(name),
    }
}

struct Window {
    since: Instant,
    frames: u64,
}

impl PumpStats {
    fn line(&self, label: &str, window: &Window) -> String {
        let secs = window.since.elapsed().as_secs_f64().max(1e-3);
        let fps = window.frames as f64 / secs;
        format!(
            "[{label}] frames={} bytes={} fps={fps:.1} last_msec={} reconnects={} codec={}",
            self.frames,
            format_bytes(self.bytes),
            self.last_msec
                .map_or_else(|| "-".to_owned(), |m| m.to_string()),
            self.reconnects,
            self.codec.map_or("?", |c| c.ffmpeg_format()),
        )
    }
}

/// Pump `events` into the sink described by `spec` until `stop` resolves,
/// the stream ends or gives up, or `stop_at_ms` is reached. The sink is
/// opened on the first video frame (codec from the codec-info frame,
/// `codec_override`, or H.264 with a warning), flushed and closed before
/// returning. A sink failure (mpv window closed, ffmpeg died, disk full)
/// is returned as the error after the stats line.
pub async fn run(
    events: &mut mpsc::Receiver<Event>,
    spec: &SinkSpec,
    opts: &PumpOptions,
    stop: impl Future<Output = ()>,
) -> Result<PumpStats> {
    let mut stats = PumpStats::default();
    let mut sink: Option<Box<dyn FrameSink>> = None;
    let mut announced: Option<Codec> = opts.codec_override;
    let mut warned_default_codec = false;
    let mut window = Window {
        since: Instant::now(),
        frames: 0,
    };
    let interval = opts.stats_interval.unwrap_or(Duration::from_secs(3600));
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(stop);

    let result: Result<()> = loop {
        let event = tokio::select! {
            biased;
            () = &mut stop => {
                stats.stopped_by = Stop::Requested;
                break Ok(());
            }
            _ = ticker.tick() => {
                if opts.stats_interval.is_some() {
                    eprintln!("{}", stats.line(&opts.label, &window));
                }
                window = Window { since: Instant::now(), frames: 0 };
                continue;
            }
            ev = events.recv() => match ev {
                Some(ev) => ev,
                None => {
                    stats.stopped_by = Stop::StreamEnded;
                    break Ok(());
                }
            },
        };
        match event {
            Event::Connected { attempt } => {
                if attempt > 1 {
                    stats.reconnects += 1;
                    tracing::info!(attempt, "reconnected");
                } else {
                    tracing::info!("connected");
                }
            }
            Event::Disconnected(why) => {
                stats.disconnects += 1;
                tracing::warn!(?why, "disconnected");
            }
            Event::GaveUp(reason) => {
                tracing::error!("stream lost: {reason}");
                stats.gave_up = Some(reason);
                stats.stopped_by = Stop::GaveUp;
                break Ok(());
            }
            Event::Stopped => {
                stats.stopped_by = Stop::StreamEnded;
                break Ok(());
            }
            Event::Frame(frame) => match frame.kind() {
                FrameKind::CodecInfo { video, audio, .. } => {
                    tracing::info!(?video, ?audio, "codec info");
                    if opts.codec_override.is_none()
                        && let Some(v) = video.as_ref()
                    {
                        match map_codec(v) {
                            Some(codec) => {
                                if let Some(current) = stats.codec
                                    && current != codec
                                {
                                    break Err(anyhow::anyhow!(
                                        "codec changed mid-stream from {current:?} to {codec:?}"
                                    ));
                                }
                                announced = Some(codec);
                            }
                            None => tracing::warn!(
                                "unknown video codec {:?}; use --codec to force one",
                                v.as_str()
                            ),
                        }
                    }
                }
                FrameKind::Video => {
                    let codec = match announced {
                        Some(c) => c,
                        None => {
                            if !warned_default_codec {
                                tracing::warn!(
                                    "video before codec info; assuming H.264 (use --codec to force)"
                                );
                                warned_default_codec = true;
                            }
                            announced = Some(Codec::H264);
                            Codec::H264
                        }
                    };
                    if sink.is_none() {
                        if let Some(path) = spec.output_path(codec) {
                            eprintln!(
                                "[{}] writing {} to {}",
                                opts.label,
                                codec.ffmpeg_format(),
                                path.display()
                            );
                        }
                        let opened = match spec.open(codec).await {
                            Ok(s) => s,
                            Err(e) => break Err(e),
                        };
                        sink = Some(opened);
                        stats.codec = Some(codec);
                    }
                    let msec = frame.header().msec();
                    let mut au = AccessUnit::new(codec, frame.payload().clone());
                    if let Some(ms) = msec {
                        au = au.with_pts_ms(ms);
                    }
                    let len = au.wire_len() as u64;
                    if let Err(e) = sink.as_mut().expect("sink opened above").push(au).await {
                        break Err(anyhow::Error::new(e).context("sink refused a frame"));
                    }
                    stats.frames += 1;
                    stats.bytes += len;
                    window.frames += 1;
                    if msec.is_some() {
                        stats.last_msec = msec;
                    }
                    if let (Some(end), Some(ms)) = (opts.stop_at_ms, msec)
                        && ms >= end
                    {
                        tracing::info!(ms, end, "end of recording reached");
                        stats.stopped_by = Stop::EndReached;
                        break Ok(());
                    }
                }
                FrameKind::Audio => stats.audio_frames += 1,
                FrameKind::Close => tracing::info!("server close notice"),
                FrameKind::Unknown => tracing::debug!("unknown frame kind"),
            },
        }
    };

    // Close the sink even on error so ffmpeg writes its trailer / mpv exits.
    let close_result = match sink.as_mut() {
        Some(s) => {
            let flushed = s.flush().await;
            let closed = s.close().await;
            flushed.and(closed)
        }
        None => Ok(()),
    };
    if opts.stats_interval.is_some() {
        eprintln!("{}", stats.line(&opts.label, &window));
    }
    result?;
    close_result.context("closing the sink")?;
    if let Some(reason) = &stats.gave_up {
        bail!("stream lost: {reason}");
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_mapping() {
        assert_eq!(map_codec(&VideoCodec::H264), Some(Codec::H264));
        assert_eq!(map_codec(&VideoCodec::H265), Some(Codec::H265));
        assert_eq!(
            map_codec(&VideoCodec::Other("hevc".into())),
            Some(Codec::H265)
        );
        assert_eq!(map_codec(&VideoCodec::Other("MJPEG".into())), None);
    }

    #[test]
    fn stats_line_mentions_every_counter() {
        let stats = PumpStats {
            frames: 12,
            bytes: 2048,
            reconnects: 1,
            last_msec: Some(500),
            codec: Some(Codec::H265),
            ..Default::default()
        };
        let w = Window {
            since: Instant::now() - Duration::from_secs(2),
            frames: 10,
        };
        let line = stats.line("live", &w);
        assert!(line.starts_with("[live] frames=12 bytes=2.0 KiB fps="));
        assert!(
            line.contains("last_msec=500 reconnects=1 codec=hevc"),
            "{line}"
        );
        let fps: f64 = line
            .split("fps=")
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .unwrap()
            .parse()
            .unwrap();
        assert!((4.0..=5.1).contains(&fps), "{fps}");
    }
}
