//! Camera audio → something a GStreamer `appsrc` can take.
//!
//! DSM's MixStream carries one audio frame per `mediaType=2` message. For
//! `adoCodec=PCMU` the payload is raw G.711 µ-law (8 kHz mono) and is passed
//! through unchanged. For `adoCodec=MPEG4-GENERIC` the payload is an AAC-LC
//! frame in one of two shapes (see [`ss_stream::aac`]); the first few frames
//! are buffered to measure the prefix length, the sample rate (from the
//! codec-info trailer, else from inter-frame timing) and the channel count,
//! then every frame gets a synthesized ADTS header so `aacparse` can
//! re-frame it. Pure data; runs on the tokio side of the bridge.

use std::time::{Duration, Instant};

use ss_stream::aac::{
    self, AacFraming, DEFAULT_CHANNELS, DEFAULT_SAMPLE_RATE, DETECTION_INTERVALS,
};
use ss_stream::{AudioCodec, Frame, FrameKind};

/// What the pipeline's audio branch has to decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AudioFormat {
    /// AAC-LC with ADTS headers (`audio/mpeg,mpegversion=4,stream-format=adts`).
    Aac,
    /// Raw G.711 µ-law, 8 kHz mono (`audio/x-mulaw`).
    Pcmu,
}

impl AudioFormat {
    /// Caps for the audio `appsrc`.
    pub fn caps_str(self) -> &'static str {
        match self {
            AudioFormat::Aac => "audio/mpeg,mpegversion=4,stream-format=adts",
            AudioFormat::Pcmu => "audio/x-mulaw,rate=8000,channels=1",
        }
    }
}

/// Frames the codec-info trailer may hand us before any audio arrives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Declared {
    channels: Option<u8>,
    sample_rate: Option<u32>,
}

/// Decided AAC parameters once detection has finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AacParams {
    framing: AacFraming,
    sample_rate: u32,
    channels: u8,
}

/// Audio frames from one stream, ready for the pipeline.
pub struct AudioTrack {
    codec: Option<AudioCodec>,
    declared: Declared,
    params: Option<AacParams>,
    /// `(header tail, payload, arrival)` while detecting.
    pending: Vec<(Vec<u8>, Vec<u8>, Instant)>,
    /// Logged once per stream when a frame cannot be framed.
    warned: bool,
}

/// Audio frames buffered before the AAC framing is decided.
pub const DETECTION_FRAMES: usize = DETECTION_INTERVALS + 1;

impl Default for AudioTrack {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioTrack {
    pub fn new() -> AudioTrack {
        AudioTrack {
            codec: None,
            declared: Declared::default(),
            params: None,
            pending: Vec::new(),
            warned: false,
        }
    }

    /// The format the pipeline needs, once the codec-info frame has arrived
    /// and names a codec this module handles.
    pub fn format(&self) -> Option<AudioFormat> {
        match self.codec.as_ref()? {
            AudioCodec::Aac => Some(AudioFormat::Aac),
            AudioCodec::Pcmu => Some(AudioFormat::Pcmu),
            AudioCodec::Other(_) => None,
        }
    }

    /// Feed one MixStream frame; returns the audio buffers to push (none for
    /// video, control frames, or AAC frames still held for detection).
    pub fn feed(&mut self, frame: &Frame) -> Vec<Vec<u8>> {
        match frame.kind() {
            FrameKind::CodecInfo {
                audio,
                audio_extra_len,
                ..
            } => {
                if audio != self.codec {
                    // a codec change restarts detection
                    self.params = None;
                    self.pending.clear();
                    self.declared = Declared::default();
                }
                if let (Some(AudioCodec::Aac), Some(n)) = (&audio, audio_extra_len)
                    && let Some((channels, rate)) =
                        aac::parse_audio_config(frame.payload(), &n.to_string())
                {
                    self.declared = Declared {
                        channels: Some(channels),
                        sample_rate: Some(rate),
                    };
                }
                self.codec = audio;
                Vec::new()
            }
            FrameKind::Audio => match self.codec {
                Some(AudioCodec::Pcmu) => vec![frame.payload().to_vec()],
                Some(AudioCodec::Aac) => self.feed_aac(
                    frame.header().tail(4).to_vec(),
                    frame.payload().to_vec(),
                    Instant::now(),
                ),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn feed_aac(&mut self, header_tail: Vec<u8>, payload: Vec<u8>, at: Instant) -> Vec<Vec<u8>> {
        if let Some(p) = self.params {
            return self.adts(p, &header_tail, &payload).into_iter().collect();
        }
        self.pending.push((header_tail, payload, at));
        if self.pending.len() < DETECTION_FRAMES {
            return Vec::new();
        }
        let p = self.decide();
        self.params = Some(p);
        tracing::info!(
            "audio: AAC {:?}, {} Hz, {} ch",
            p.framing,
            p.sample_rate,
            p.channels
        );
        let pending = std::mem::take(&mut self.pending);
        pending
            .iter()
            .filter_map(|(tail, payload, _)| self.adts(p, tail, payload))
            .collect()
    }

    fn decide(&self) -> AacParams {
        let payloads: Vec<&[u8]> = self.pending.iter().map(|(_, p, _)| p.as_slice()).collect();
        let framing = match aac::detect_frame_prefix_len(&payloads) {
            Some(n) => AacFraming::PayloadPrefix(n),
            None => AacFraming::HeaderPrepend,
        };
        let raw: Vec<Vec<u8>> = self
            .pending
            .iter()
            .map(|(tail, payload, _)| aac::reconstruct_frame(framing, tail, payload))
            .collect();
        let channels =
            aac::detect_channel_count(&raw, self.declared.channels.unwrap_or(DEFAULT_CHANNELS));
        let intervals: Vec<Duration> = self
            .pending
            .windows(2)
            .map(|w| w[1].2.saturating_duration_since(w[0].2))
            .collect();
        let sample_rate = self
            .declared
            .sample_rate
            .or_else(|| aac::sample_rate_from_intervals(&intervals))
            .unwrap_or(DEFAULT_SAMPLE_RATE);
        AacParams {
            framing,
            sample_rate,
            channels,
        }
    }

    fn adts(&mut self, p: AacParams, header_tail: &[u8], payload: &[u8]) -> Option<Vec<u8>> {
        let raw = aac::reconstruct_frame(p.framing, header_tail, payload);
        if raw.is_empty() {
            return None;
        }
        match aac::adts_frame(&raw, p.sample_rate, p.channels) {
            Ok(f) => Some(f),
            Err(e) => {
                if !self.warned {
                    self.warned = true;
                    tracing::warn!("audio: {e}");
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ss_stream::parse_frame;

    fn frame(header: &[u8], payload: &[u8]) -> Frame {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(header.len() as u32).to_be_bytes());
        bytes.extend_from_slice(header);
        bytes.extend_from_slice(payload);
        parse_frame(&bytes).expect("fixture frame")
    }

    /// A 3-byte DSM prefix (its last byte reads as AAC `ID_END`, which is
    /// what lets the detector rule out a 2-byte prefix) in front of a
    /// channel_pair_element block.
    fn prefixed(i: u8) -> Vec<u8> {
        let mut v = vec![0x34, 0x1f, 0xe0 | (i & 0x1f)];
        v.push(0b001 << 5);
        v.extend(std::iter::repeat_n(0xaa, 19));
        v
    }

    #[test]
    fn pcmu_passes_through() {
        let mut t = AudioTrack::new();
        assert!(
            t.feed(&frame(b"vdoCodec=H264&adoCodec=PCMU", b""))
                .is_empty()
        );
        assert_eq!(t.format(), Some(AudioFormat::Pcmu));
        let out = t.feed(&frame(b"mediaType=2&msec=1", &[0xff, 0x7f, 0x00]));
        assert_eq!(out, vec![vec![0xff, 0x7f, 0x00]]);
    }

    #[test]
    fn aac_buffers_then_frames_with_adts() {
        let mut t = AudioTrack::new();
        let mut info = b"\x00\x00\x00\x01\x67\x64\x00\x33".to_vec();
        info.extend_from_slice(b"2|16000|");
        t.feed(&frame(
            b"vdoCodec=H264&adoCodec=MPEG4-GENERIC&adoExtra=8",
            &info,
        ));
        assert_eq!(t.format(), Some(AudioFormat::Aac));
        let mut out = Vec::new();
        for i in 0..DETECTION_FRAMES as u8 {
            out.extend(t.feed(&frame(b"mediaType=2&msec=5", &prefixed(i))));
        }
        assert_eq!(out.len(), DETECTION_FRAMES);
        // 7-byte header + 20 raw bytes, sync word, 16 kHz index 8, 2 ch
        assert_eq!(out[0].len(), 27);
        assert_eq!(out[0][0], 0xff);
        assert_eq!((out[0][2] >> 2) & 0xf, 8);
        assert_eq!(out[0][7], 0b001 << 5);
        // a later frame is framed immediately
        assert_eq!(t.feed(&frame(b"mediaType=2", &prefixed(9))).len(), 1);
    }

    #[test]
    fn unknown_codecs_yield_nothing() {
        let mut t = AudioTrack::new();
        t.feed(&frame(b"vdoCodec=H264&adoCodec=G726", b""));
        assert_eq!(t.format(), None);
        assert!(t.feed(&frame(b"mediaType=2", &[1, 2, 3])).is_empty());
        // video frames are never audio
        assert!(
            t.feed(&frame(b"mediaType=1", &[0, 0, 0, 1, 0x65]))
                .is_empty()
        );
    }
}
