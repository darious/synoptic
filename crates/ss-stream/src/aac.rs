//! AAC frame reconstruction for `adoCodec=MPEG4-GENERIC` audio.
//!
//! Ported from the reference's `services/aac.py`: the pure byte-level
//! parts only. DSM never sends a self-contained ADTS frame. Two camera
//! shapes are known:
//!
//! 1. **Payload prefix**: the payload is the raw AAC frame behind a short
//!    prefix (2 or 3 bytes seen; the 3-byte one is the tail of an ADTS
//!    header). [`detect_frame_prefix_len`] measures the length from a few
//!    frames, [`strip_frame_prefix`] removes it.
//! 2. **Header prepend** (Reolink RLC-823A): the payload is missing its own
//!    leading bytes, which are exactly the last bytes of the WebSocket
//!    message's ASCII header ([`crate::frame::Header::tail`]`(4)`).
//!
//! [`reconstruct_frame`] applies either; [`adts_header`] then synthesizes
//! the 7-byte ADTS header a plain AAC demuxer needs. Sample rate comes
//! from the codec-info trailer ([`parse_audio_config`]) when a camera
//! sends one, otherwise from inter-frame timing
//! ([`sample_rate_from_intervals`]); channels from the first frame that
//! names a layout ([`detect_channel_count`]).
//!
//! **Not ported** (needs an `ffmpeg` subprocess and a player pipeline):
//! `AacDetector.frames_look_valid`/`finish`, which trial-decodes the
//! buffered frames under each framing with ffmpeg to pick between the two
//! shapes above, and the video/audio buffering around detection. A sink
//! crate that owns ffmpeg should do that: try [`AacFraming::PayloadPrefix`]
//! with the detected length first, then [`AacFraming::HeaderPrepend`].

use std::time::Duration;

/// Samples per AAC-LC frame.
pub const SAMPLES_PER_FRAME: u32 = 1024;
/// Rates a measured interval is snapped to.
pub const STANDARD_SAMPLE_RATES: [u32; 9] =
    [8000, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000];
/// Default sample rate before anything is known (what the reference assumed).
pub const DEFAULT_SAMPLE_RATE: u32 = 16000;
/// Default channel count before anything is known.
pub const DEFAULT_CHANNELS: u8 = 2;
/// How many inter-frame intervals the reference collects (odd, for a median).
pub const DETECTION_INTERVALS: usize = 11;

const ADTS_MAX_FRAME_LEN: usize = 0x1FFF;
const PREFIX_MIN_LEN: usize = 2;
const PREFIX_MAX_LEN: usize = 8;
const AAC_ELEMENT_ID_END: u8 = 0b111;

fn adts_freq_index(sample_rate: u32) -> Option<u8> {
    Some(match sample_rate {
        96000 => 0,
        88200 => 1,
        64000 => 2,
        48000 => 3,
        44100 => 4,
        32000 => 5,
        24000 => 6,
        22050 => 7,
        16000 => 8,
        12000 => 9,
        11025 => 10,
        8000 => 11,
        7350 => 12,
        _ => return None,
    })
}

/// How to recover the raw AAC frame from one WebSocket message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AacFraming {
    /// Strip this many leading payload bytes.
    PayloadPrefix(usize),
    /// Prepend the last four header bytes to the payload.
    HeaderPrepend,
}

/// Remove DSM's leading prefix, leaving the raw AAC frame.
pub fn strip_frame_prefix(frame: &[u8], prefix_len: usize) -> &[u8] {
    &frame[prefix_len.min(frame.len())..]
}

/// Recover one raw AAC frame. `header_tail` is
/// [`crate::frame::Header::tail`]`(4)` of the same message.
pub fn reconstruct_frame(framing: AacFraming, header_tail: &[u8], payload: &[u8]) -> Vec<u8> {
    match framing {
        AacFraming::HeaderPrepend => [header_tail, payload].concat(),
        AacFraming::PayloadPrefix(n) => strip_frame_prefix(payload, n).to_vec(),
    }
}

/// Work out how many leading bytes DSM puts in front of the raw AAC frame,
/// from a handful of real (still prefixed) payloads.
///
/// Eliminates rather than confirms: a candidate length whose first
/// post-strip byte reads as AAC's `ID_END` element on any frame is
/// provably wrong; the shortest survivor in 2..=8 wins. Frames of 8 bytes
/// or fewer are ignored rather than allowed to veto. `None` if nothing
/// usable is left or no candidate holds (the header-prepend camera).
pub fn detect_frame_prefix_len<F: AsRef<[u8]>>(frames: &[F]) -> Option<usize> {
    let usable: Vec<&[u8]> = frames
        .iter()
        .map(AsRef::as_ref)
        .filter(|f| f.len() > PREFIX_MAX_LEN)
        .collect();
    if usable.is_empty() {
        return None;
    }
    (PREFIX_MIN_LEN..=PREFIX_MAX_LEN)
        .find(|&len| usable.iter().all(|f| (f[len] >> 5) != AAC_ELEMENT_ID_END))
}

/// The channel count named by the first reconstructed frame that opens
/// with a single_channel_element (1) or channel_pair_element (2);
/// `declared` (from [`parse_audio_config`], else stereo) if none does.
/// Synology's own decoder resolves it in the same order.
pub fn detect_channel_count<F: AsRef<[u8]>>(frames: &[F], declared: u8) -> u8 {
    frames
        .iter()
        .map(AsRef::as_ref)
        .filter(|f| !f.is_empty())
        .find_map(|f| match f[0] >> 5 {
            0b000 => Some(1),
            0b001 => Some(2),
            _ => None,
        })
        .unwrap_or(declared)
}

/// Snap a measured inter-frame interval to the nearest standard AAC
/// sample rate (1024 samples per frame). Non-positive → 16 kHz.
pub fn nearest_sample_rate(interval: Duration) -> u32 {
    let secs = interval.as_secs_f64();
    if secs <= 0.0 {
        return DEFAULT_SAMPLE_RATE;
    }
    let measured = f64::from(SAMPLES_PER_FRAME) / secs;
    STANDARD_SAMPLE_RATES
        .into_iter()
        .min_by(|a, b| {
            (f64::from(*a) - measured)
                .abs()
                .total_cmp(&(f64::from(*b) - measured).abs())
        })
        .unwrap_or(DEFAULT_SAMPLE_RATE)
}

/// The sample rate from the **median** of real inter-frame arrival
/// intervals (the reference skips intervals that are zero or ≥ 0.5 s, as
/// those span a reconnect gap). `None` with no usable interval.
pub fn sample_rate_from_intervals(intervals: &[Duration]) -> Option<u32> {
    let mut usable: Vec<Duration> = intervals
        .iter()
        .copied()
        .filter(|d| !d.is_zero() && *d < Duration::from_millis(500))
        .collect();
    if usable.is_empty() {
        return None;
    }
    usable.sort();
    let mid = usable.len() / 2;
    let median = if usable.len() % 2 == 1 {
        usable[mid]
    } else {
        (usable[mid - 1] + usable[mid]) / 2
    };
    Some(nearest_sample_rate(median))
}

/// Decode the audio trailer DSM appends to the codec-info frame's
/// payload: the last `extra_len` bytes (`adoExtra`, still a string as it
/// comes from the header) hold ASCII `<channels>|<sampleRate>|`,
/// optionally followed by a raw 2-byte AAC AudioSpecificConfig.
/// Confirmed on three cameras (two AAC, one PCMU).
///
/// Returns `(channels, sample_rate)`, or `None` if `adoExtra` is missing
/// or not a number, the payload is shorter than claimed, or the trailer
/// is not this shape; callers fall back to runtime detection.
pub fn parse_audio_config(payload: &[u8], extra_len: &str) -> Option<(u8, u32)> {
    if extra_len.is_empty() || !extra_len.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: usize = extra_len.parse().ok()?;
    if n == 0 || n > payload.len() {
        return None;
    }
    let trailer = &payload[payload.len() - n..];
    let mut parts = trailer.splitn(3, |&b| b == b'|');
    let channels = std::str::from_utf8(parts.next()?)
        .ok()?
        .parse::<u8>()
        .ok()?;
    let sample_rate = std::str::from_utf8(parts.next()?)
        .ok()?
        .parse::<u32>()
        .ok()?;
    if channels == 0 || adts_freq_index(sample_rate).is_none() {
        return None;
    }
    Some((channels, sample_rate))
}

/// Why [`adts_header`] refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AdtsError {
    /// Not in the ADTS sampling-frequency table.
    #[error("sample rate {0} has no ADTS frequency index")]
    UnsupportedSampleRate(u32),
    /// Header + payload would not fit the 13-bit length field.
    #[error("AAC frame of {0} bytes exceeds what an ADTS header can describe")]
    FrameTooLong(usize),
}

/// A 7-byte ADTS header (no CRC, AAC-LC) for a raw frame of
/// `payload_length` bytes. Lets a plain AAC demuxer find frames via the
/// sync word. A wrong channel count is silent downstream, so take it from
/// [`detect_channel_count`].
pub fn adts_header(
    payload_length: usize,
    sample_rate: u32,
    channels: u8,
) -> Result<[u8; 7], AdtsError> {
    let freq_idx =
        adts_freq_index(sample_rate).ok_or(AdtsError::UnsupportedSampleRate(sample_rate))?;
    let frame_len = payload_length + 7;
    if frame_len > ADTS_MAX_FRAME_LEN {
        return Err(AdtsError::FrameTooLong(payload_length));
    }
    let profile_id: u8 = 1; // AAC-LC (object type 2) -> profile = object_type - 1
    let fl = frame_len as u32;
    Ok([
        0xFF,
        0xF1,
        ((profile_id & 0x3) << 6) | ((freq_idx & 0xF) << 2) | ((channels >> 2) & 0x1),
        ((channels & 0x3) << 6) | ((fl >> 11) & 0x3) as u8,
        ((fl >> 3) & 0xFF) as u8,
        (((fl & 0x7) << 5) as u8) | 0x1F,
        0xFC,
    ])
}

/// Prepend an ADTS header to a reconstructed frame.
pub fn adts_frame(raw: &[u8], sample_rate: u32, channels: u8) -> Result<Vec<u8>, AdtsError> {
    let h = adts_header(raw.len(), sample_rate, channels)?;
    let mut out = Vec::with_capacity(7 + raw.len());
    out.extend_from_slice(&h);
    out.extend_from_slice(raw);
    Ok(out)
}

#[cfg(test)]
mod tests {
    //! Ported from the reference's `tests/test_aac.py`, fixtures included.
    use super::*;

    fn parse_adts(h: &[u8; 7]) -> (u8, u8, usize) {
        let freq_idx = (h[2] >> 2) & 0xF;
        let channels = ((h[2] & 0x1) << 2) | ((h[3] >> 6) & 0x3);
        let frame_len =
            ((h[3] as usize & 0x3) << 11) | ((h[4] as usize) << 3) | ((h[5] as usize >> 5) & 0x7);
        (freq_idx, channels, frame_len)
    }

    const SCE: u8 = 0;
    const CPE: u8 = 1;
    const FIL: u8 = 6;
    const END: u8 = 0b111;

    fn element(id: u8) -> u8 {
        (id & 0x7) << 5
    }
    fn raw_block(len: usize, id: u8) -> Vec<u8> {
        let mut v = vec![element(id)];
        v.extend(std::iter::repeat_n(0xaa, len - 1));
        v
    }
    fn prefixed_frame(prefix: &[u8], first: u8) -> Vec<u8> {
        let mut v = prefix.to_vec();
        v.push(first);
        v.extend(std::iter::repeat_n(0xaa, 19));
        v
    }
    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn adts_round_trips_rate_and_length() {
        for (rate, idx) in [(8000, 11), (16000, 8), (44100, 4), (48000, 3)] {
            let h = adts_header(413, rate, 2).unwrap();
            assert_eq!(parse_adts(&h), (idx, 2, 420));
        }
        let h = adts_header(100, 8000, 2).unwrap();
        assert_eq!(h[0], 0xFF);
        assert_eq!(h[1] & 0xF0, 0xF0);
    }

    #[test]
    fn adts_writes_channel_count_and_rejects_overlong() {
        assert_eq!(parse_adts(&adts_header(413, 16000, 1).unwrap()).1, 1);
        assert_eq!(parse_adts(&adts_header(413, 8000, 2).unwrap()).1, 2);
        assert_eq!(parse_adts(&adts_header(8184, 16000, 2).unwrap()).2, 8191);
        assert_eq!(
            adts_header(8185, 16000, 2),
            Err(AdtsError::FrameTooLong(8185))
        );
        assert_eq!(
            adts_header(10, 99999, 2),
            Err(AdtsError::UnsupportedSampleRate(99999))
        );
        assert_eq!(adts_frame(&[1, 2, 3], 16000, 1).unwrap().len(), 10);
    }

    #[test]
    fn channel_count_rules() {
        assert_eq!(detect_channel_count(&[raw_block(413, SCE)], 2), 1);
        assert_eq!(detect_channel_count(&[raw_block(413, CPE)], 2), 2);
        let frames = [raw_block(20, FIL), raw_block(413, SCE), raw_block(413, SCE)];
        assert_eq!(detect_channel_count(&frames, 2), 1);
        assert_eq!(detect_channel_count(&[vec![], raw_block(20, FIL)], 2), 2);
        assert_eq!(detect_channel_count::<Vec<u8>>(&[], 2), 2);
        assert_eq!(detect_channel_count(&[vec![], raw_block(20, FIL)], 1), 1);
        assert_eq!(detect_channel_count(&[raw_block(413, SCE)], 2), 1);
        assert_eq!(detect_channel_count(&[raw_block(413, CPE)], 1), 2);
    }

    #[test]
    fn nearest_rate_snaps_and_tolerates_jitter() {
        assert_eq!(
            nearest_sample_rate(Duration::from_secs_f64(1024.0 / 8000.0)),
            8000
        );
        assert_eq!(
            nearest_sample_rate(Duration::from_secs_f64(1024.0 / 16000.0)),
            16000
        );
        assert_eq!(
            nearest_sample_rate(Duration::from_secs_f64(1024.0 / 44100.0)),
            44100
        );
        assert_eq!(nearest_sample_rate(Duration::from_secs_f64(0.113)), 8000);
        assert_eq!(nearest_sample_rate(Duration::from_secs_f64(0.145)), 8000);
        assert!(nearest_sample_rate(Duration::ZERO) > 0);
    }

    #[test]
    fn median_survives_a_scheduling_hiccup() {
        for rate in [16000u32, 22050, 32000, 44100, 48000] {
            let nominal = Duration::from_secs_f64(1024.0 / f64::from(rate));
            let mut intervals = vec![nominal; DETECTION_INTERVALS - 1];
            intervals.push(nominal + Duration::from_millis(50));
            assert_eq!(sample_rate_from_intervals(&intervals), Some(rate));
        }
        assert_eq!(sample_rate_from_intervals(&[]), None);
        assert_eq!(sample_rate_from_intervals(&[Duration::from_secs(2)]), None);
        const { assert!(DETECTION_INTERVALS >= 3 && DETECTION_INTERVALS % 2 == 1) };
    }

    #[test]
    fn strip_and_reconstruct() {
        let frame = [0u8, 1, 2, 3, 4];
        assert_eq!(strip_frame_prefix(&frame, 3), &[3, 4]);
        assert_eq!(strip_frame_prefix(&frame, 2), &[2, 3, 4]);
        assert_eq!(strip_frame_prefix(&frame, 9), &[] as &[u8]);
        assert_eq!(
            reconstruct_frame(AacFraming::PayloadPrefix(2), b"HEAD", &frame),
            vec![2, 3, 4]
        );
        assert_eq!(
            reconstruct_frame(AacFraming::HeaderPrepend, &[9, 8, 7, 6], &frame),
            vec![9, 8, 7, 6, 0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn prefix_len_common_two_bytes() {
        let frames: Vec<Vec<u8>> = (0..6u8)
            .map(|i| prefixed_frame(&[0x34, i], element(CPE)))
            .collect();
        assert_eq!(detect_frame_prefix_len(&frames), Some(2));
    }

    #[test]
    fn prefix_len_real_capture_regression() {
        let real = [
            hex("341ffc211a1483320ccf8320"),
            hex("34fffc211a14830e06c681b0"),
            hex("339ffc211a148b5606c684b0"),
            hex("31bffc211a148b7c50660b59"),
            hex("345ffc211a14832606d284b0"),
        ];
        assert_eq!(detect_frame_prefix_len(&real), Some(3));
    }

    #[test]
    fn prefix_len_edge_cases() {
        let prefix = [0x00, 0x00, element(END), element(END)];
        let frames: Vec<Vec<u8>> = (0..6)
            .map(|_| prefixed_frame(&prefix, element(CPE)))
            .collect();
        assert_eq!(detect_frame_prefix_len(&frames), Some(4));

        let mut frames: Vec<Vec<u8>> = (0..5)
            .map(|_| prefixed_frame(&[0x34, 0x1F, 0x00], element(CPE)))
            .collect();
        frames.push(prefixed_frame(&[0x34, 0x1F, element(END)], element(CPE)));
        assert_eq!(detect_frame_prefix_len(&frames), Some(3));

        let mut frames: Vec<Vec<u8>> = (0..6)
            .map(|_| prefixed_frame(&[0x34, 0x1F], element(CPE)))
            .collect();
        frames.push(vec![0x00]);
        assert_eq!(detect_frame_prefix_len(&frames), Some(2));

        let frames: Vec<Vec<u8>> = (0..6).map(|_| vec![element(END); 10]).collect();
        assert_eq!(detect_frame_prefix_len(&frames), None);
        assert_eq!(detect_frame_prefix_len(&[vec![0u8], vec![1u8]]), None);
        assert_eq!(detect_frame_prefix_len::<Vec<u8>>(&[]), None);
    }

    #[test]
    fn audio_config_real_captures() {
        let mut p = b"\x00\x00\x00\x01\x67\x64\x00\x33".to_vec();
        p.extend_from_slice(b"1|16000|");
        p.extend_from_slice(&hex("1408"));
        assert_eq!(parse_audio_config(&p, "10"), Some((1, 16000)));

        let mut p = b"\x00\x00\x00\x01\x67\x4d\x00\x32".to_vec();
        p.extend_from_slice(b"2|8000|");
        p.extend_from_slice(&hex("1590"));
        assert_eq!(parse_audio_config(&p, "9"), Some((2, 8000)));

        let mut p = b"\x00\x00\x00\x01\x67\x4d\x00\x32".to_vec();
        p.extend_from_slice(b"1|8000|");
        assert_eq!(parse_audio_config(&p, "7"), Some((1, 8000)));
    }

    #[test]
    fn audio_config_rejections() {
        let t = [b"1|16000|".as_slice(), &hex("1408")].concat();
        assert_eq!(parse_audio_config(&t, ""), None);
        assert_eq!(parse_audio_config(&t, "ten"), None);
        assert_eq!(parse_audio_config(b"1|16000|", "999"), None);
        assert_eq!(parse_audio_config(&t, "0"), None);
        assert_eq!(parse_audio_config(&[0xaa; 10], "10"), None);
        assert_eq!(parse_audio_config(b"one|16000|", "10"), None);
        assert_eq!(parse_audio_config(b"1|sixteen|", "10"), None);
        assert_eq!(parse_audio_config(b"1|99999|", "8"), None);
    }
}
