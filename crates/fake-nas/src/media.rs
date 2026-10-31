//! The fixture media: `test.h264` split into access units, `test.aac`
//! split into ADTS frames, `test.mp4` and `snap.jpg` served as downloads.
//!
//! The MixStream payload for one video frame is the access unit's NAL
//! units joined by 4-byte Annex-B start codes **without the leading start
//! code**: `ss_stream::Frame::annex_b` prepends exactly one, which makes
//! the result a complete Annex-B access unit (SPS, PPS and every slice of
//! the picture). Access units are cut at a slice whose `first_mb_in_slice`
//! is 0 (a new primary coded picture), with any SPS/PPS/SEI in front of it
//! belonging to that picture, so a multi-slice encoder output groups
//! correctly.
//!
//! Audio payloads follow the shape the reference client documents for
//! `adoCodec=MPEG4-GENERIC`: the raw AAC frame behind a 3-byte prefix
//! that is the tail of its ADTS header (`ss_stream::aac`).

use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine;
use bytes::Bytes;

/// Frame rate of `test.h264`.
pub const VIDEO_FPS: u32 = 15;
/// One video frame in milliseconds.
pub const VIDEO_FRAME_MS: f64 = 1000.0 / VIDEO_FPS as f64;
/// Samples per AAC-LC frame.
pub const AAC_SAMPLES_PER_FRAME: u32 = 1024;

const START_CODE: [u8; 4] = [0, 0, 0, 1];
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_IDR: u8 = 5;

/// A 16x9 grey JPEG, used when `snap.jpg` is missing and `ffmpeg` cannot
/// make one.
pub const PLACEHOLDER_JPEG: &[u8] = &[
    0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x02, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0xff, 0xfe, 0x00, 0x0f, 0x4c, 0x61, 0x76, 0x63, 0x36, 0x33, 0x2e, 0x31,
    0x2e, 0x31, 0x30, 0x32, 0x00, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x08, 0x3e, 0x3e, 0x49, 0x3e, 0x49,
    0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x64, 0x5d, 0x64, 0x68, 0x68, 0x68, 0x64, 0x64, 0x64, 0x64,
    0x68, 0x68, 0x68, 0x70, 0x70, 0x70, 0x83, 0x83, 0x83, 0x70, 0x70, 0x70, 0x68, 0x68, 0x70, 0x70,
    0x7c, 0x7c, 0x83, 0x83, 0x8f, 0x93, 0x8f, 0x87, 0x87, 0x83, 0x87, 0x93, 0x93, 0x9b, 0x9b, 0x9b,
    0xba, 0xba, 0xb2, 0xb2, 0xd9, 0xd9, 0xe0, 0xff, 0xff, 0xff, 0xff, 0xc4, 0x00, 0x4b, 0x00, 0x01,
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x07, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x11, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x09, 0x00, 0x10,
    0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00, 0xff, 0xda, 0x00, 0x0c, 0x03, 0x01,
    0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3f, 0x00, 0x9f, 0x80, 0x0f, 0xff, 0xd9,
];

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}: no H.264 access units found (expected an Annex-B elementary stream)")]
    NoVideo(PathBuf),
    #[error("{0}: no ADTS AAC frames found")]
    NoAudio(PathBuf),
}

/// One video frame as MixStream carries it.
#[derive(Clone, Debug)]
pub struct AccessUnit {
    /// NALs joined by start codes, no leading start code.
    pub payload: Bytes,
    /// Contains an IDR slice (and, from x264, the SPS/PPS in front of it).
    pub keyframe: bool,
}

pub struct Media {
    pub video: Vec<AccessUnit>,
    /// Annex-B SPS + PPS, for the codec-info frame.
    pub parameter_sets: Bytes,
    /// Prefixed raw AAC frames.
    pub audio: Vec<Bytes>,
    pub audio_channels: u8,
    pub audio_sample_rate: u32,
    pub mp4: Bytes,
    pub jpeg: Bytes,
    /// Base64 of a small JPEG, for inline `imageData` / `thumbnail` fields.
    pub thumbnail_b64: String,
    /// Frames between keyframes.
    pub gop: usize,
}

impl std::fmt::Debug for Media {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Media")
            .field("video_aus", &self.video.len())
            .field("gop", &self.gop)
            .field("audio_frames", &self.audio.len())
            .field("audio_sample_rate", &self.audio_sample_rate)
            .field("mp4_bytes", &self.mp4.len())
            .field("jpeg_bytes", &self.jpeg.len())
            .finish()
    }
}

fn read(path: &Path) -> Result<Vec<u8>, MediaError> {
    std::fs::read(path).map_err(|source| MediaError::Io {
        path: path.to_path_buf(),
        source,
    })
}

impl Media {
    /// Load `test.h264`, `test.aac`, `test.mp4` and `snap.jpg` from `dir`,
    /// making `snap.jpg` (and a smaller `thumb.jpg`) with `ffmpeg` when
    /// they are missing.
    pub fn load(dir: &Path) -> Result<Media, MediaError> {
        let h264 = dir.join("test.h264");
        let h264_bytes = read(&h264)?;
        let nals = split_nals(&h264_bytes);
        let (video, parameter_sets) = group_access_units(&nals);
        if video.is_empty() {
            return Err(MediaError::NoVideo(h264));
        }
        let gop = video
            .iter()
            .enumerate()
            .filter(|(_, au)| au.keyframe)
            .map(|(i, _)| i)
            .nth(1)
            .unwrap_or(video.len())
            .max(1);

        let aac = dir.join("test.aac");
        let aac_bytes = read(&aac)?;
        let (audio, audio_channels, audio_sample_rate) = split_adts(&aac_bytes);
        if audio.is_empty() {
            return Err(MediaError::NoAudio(aac));
        }

        let mp4 = Bytes::from(read(&dir.join("test.mp4"))?);
        let (jpeg, thumb) = ensure_snapshots(dir);
        let thumbnail_b64 = base64::engine::general_purpose::STANDARD.encode(&thumb);
        Ok(Media {
            video,
            parameter_sets,
            audio,
            audio_channels,
            audio_sample_rate,
            mp4,
            jpeg,
            thumbnail_b64,
            gop,
        })
    }

    /// One audio frame in milliseconds.
    pub fn audio_frame_ms(&self) -> f64 {
        f64::from(AAC_SAMPLES_PER_FRAME) * 1000.0 / f64::from(self.audio_sample_rate)
    }

    /// The video access unit for frame number `k` of an endless loop.
    pub fn video_frame(&self, k: i64) -> &AccessUnit {
        let n = self.video.len() as i64;
        &self.video[k.rem_euclid(n) as usize]
    }

    /// The audio frame for frame number `j` of an endless loop.
    pub fn audio_frame(&self, j: i64) -> &Bytes {
        let n = self.audio.len() as i64;
        &self.audio[j.rem_euclid(n) as usize]
    }

    /// The 2-byte AAC `AudioSpecificConfig` (AAC-LC) for the fixture.
    pub fn audio_specific_config(&self) -> [u8; 2] {
        let freq_idx = adts_freq_index(self.audio_sample_rate).unwrap_or(3);
        let object_type = 2u8;
        [
            (object_type << 3) | (freq_idx >> 1),
            ((freq_idx & 1) << 7) | ((self.audio_channels & 0xF) << 3),
        ]
    }
}

/// Split an Annex-B byte stream into NAL units (start codes removed,
/// trailing zero bytes trimmed).
pub fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::with_capacity(starts.len());
    for (n, &s) in starts.iter().enumerate() {
        let mut e = starts.get(n + 1).map_or(data.len(), |&next| next - 3);
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        if e > s {
            nals.push(&data[s..e]);
        }
    }
    nals
}

fn nal_type(nal: &[u8]) -> u8 {
    nal[0] & 0x1F
}

fn is_vcl(nal: &[u8]) -> bool {
    (1..=5).contains(&nal_type(nal))
}

/// `first_mb_in_slice == 0`: the first slice of a picture. The field is
/// the first `ue(v)` of the slice header, and 0 is the single bit `1`.
fn starts_picture(nal: &[u8]) -> bool {
    nal.len() > 1 && nal[1] & 0x80 != 0
}

/// Group NAL units into access units; also return the first SPS+PPS pair
/// as an Annex-B blob.
pub fn group_access_units(nals: &[&[u8]]) -> (Vec<AccessUnit>, Bytes) {
    let mut aus = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut cur_has_vcl = false;
    let mut cur_key = false;
    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;

    let flush = |cur: &mut Vec<u8>, key: bool, aus: &mut Vec<AccessUnit>| {
        if !cur.is_empty() {
            aus.push(AccessUnit {
                payload: Bytes::from(std::mem::take(cur)),
                keyframe: key,
            });
        }
    };

    for &nal in nals {
        let t = nal_type(nal);
        if t == NAL_SPS && sps.is_none() {
            sps = Some(nal.to_vec());
        }
        if t == NAL_PPS && pps.is_none() {
            pps = Some(nal.to_vec());
        }
        let vcl = is_vcl(nal);
        let begins_new = if vcl { starts_picture(nal) } else { true };
        if cur_has_vcl && begins_new {
            flush(&mut cur, cur_key, &mut aus);
            cur_has_vcl = false;
            cur_key = false;
        }
        if !cur.is_empty() {
            cur.extend_from_slice(&START_CODE);
        }
        cur.extend_from_slice(nal);
        if vcl {
            cur_has_vcl = true;
            if t == NAL_IDR {
                cur_key = true;
            }
        }
    }
    if cur_has_vcl {
        flush(&mut cur, cur_key, &mut aus);
    }

    let mut ps = Vec::new();
    for nal in [sps, pps].into_iter().flatten() {
        ps.extend_from_slice(&START_CODE);
        ps.extend_from_slice(&nal);
    }
    (aus, Bytes::from(ps))
}

fn adts_freq_index(sample_rate: u32) -> Option<u8> {
    ADTS_RATES
        .iter()
        .position(|&r| r == sample_rate)
        .map(|i| i as u8)
}

const ADTS_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

/// Split an ADTS stream into MixStream audio payloads (3-byte header tail
/// + raw AAC). Returns `(frames, channels, sample_rate)`.
pub fn split_adts(data: &[u8]) -> (Vec<Bytes>, u8, u32) {
    let mut frames = Vec::new();
    let mut channels = 1u8;
    let mut rate = 48000u32;
    let mut i = 0;
    while i + 7 <= data.len() {
        let h = &data[i..i + 7];
        if h[0] != 0xFF || h[1] & 0xF0 != 0xF0 {
            i += 1;
            continue;
        }
        let protection_absent = h[1] & 1 == 1;
        let header_len = if protection_absent { 7 } else { 9 };
        let frame_len =
            ((h[3] as usize & 0x3) << 11) | ((h[4] as usize) << 3) | ((h[5] as usize) >> 5);
        if frame_len < header_len || i + frame_len > data.len() {
            break;
        }
        if frames.is_empty() {
            let freq_idx = (h[2] >> 2) & 0xF;
            rate = ADTS_RATES.get(freq_idx as usize).copied().unwrap_or(48000);
            channels = ((h[2] & 1) << 2) | (h[3] >> 6);
            if channels == 0 {
                channels = 2;
            }
        }
        let mut payload = Vec::with_capacity(3 + frame_len - header_len);
        payload.extend_from_slice(&h[4..7]);
        payload.extend_from_slice(&data[i + header_len..i + frame_len]);
        frames.push(Bytes::from(payload));
        i += frame_len;
    }
    (frames, channels, rate)
}

/// `snap.jpg` and `thumb.jpg` from `dir`, created with `ffmpeg` from
/// `test.mp4` when missing; the embedded placeholder when that fails.
fn ensure_snapshots(dir: &Path) -> (Bytes, Bytes) {
    let snap = dir.join("snap.jpg");
    let thumb = dir.join("thumb.jpg");
    let mp4 = dir.join("test.mp4");
    if !snap.exists() {
        ffmpeg(&["-y", "-i"], &mp4, &["-frames:v", "1", "-q:v", "3"], &snap);
    }
    if !thumb.exists() {
        ffmpeg(
            &["-y", "-i"],
            &mp4,
            &["-frames:v", "1", "-vf", "scale=320:-2", "-q:v", "6"],
            &thumb,
        );
    }
    let jpeg = std::fs::read(&snap).map(Bytes::from).unwrap_or_else(|_| {
        tracing::warn!(
            "no {} and ffmpeg could not make one; using placeholder",
            snap.display()
        );
        Bytes::from_static(PLACEHOLDER_JPEG)
    });
    let thumb = std::fs::read(&thumb)
        .map(Bytes::from)
        .unwrap_or_else(|_| jpeg.clone());
    (jpeg, thumb)
}

fn ffmpeg(pre: &[&str], input: &Path, post: &[&str], out: &Path) {
    let status = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .args(pre)
        .arg(input)
        .args(post)
        .arg(out)
        .status();
    match status {
        Ok(s) if s.success() => tracing::info!("created {}", out.display()),
        Ok(s) => tracing::warn!("ffmpeg exited with {s} while creating {}", out.display()),
        Err(e) => tracing::warn!("ffmpeg not runnable ({e}); cannot create {}", out.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(t: u8, first_mb_zero: bool, body: &[u8]) -> Vec<u8> {
        let mut v = vec![0x60 | t, if first_mb_zero { 0xB0 } else { 0x40 }];
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn splits_and_groups_multi_slice_pictures() {
        let mut stream = Vec::new();
        for (sc, n) in [
            (&[0u8, 0, 0, 1][..], nal(7, true, b"sps")),
            (&[0, 0, 0, 1], nal(8, true, b"pps")),
            (&[0, 0, 1], nal(5, true, b"idr0")),
            (&[0, 0, 0, 1], nal(5, false, b"idr1")),
            (&[0, 0, 0, 1], nal(1, true, b"p0")),
            (&[0, 0, 1], nal(1, false, b"p1")),
            (&[0, 0, 0, 1], nal(1, true, b"q0")),
        ] {
            stream.extend_from_slice(sc);
            stream.extend_from_slice(&n);
        }
        let nals = split_nals(&stream);
        assert_eq!(nals.len(), 7);
        let (aus, ps) = group_access_units(&nals);
        assert_eq!(aus.len(), 3);
        assert!(aus[0].keyframe && !aus[1].keyframe && !aus[2].keyframe);
        // SPS, PPS, two IDR slices joined by start codes, none leading.
        let p = &aus[0].payload;
        assert_eq!(p[0] & 0x1F, 7);
        assert_eq!(p.windows(4).filter(|w| *w == [0, 0, 0, 1]).count(), 3);
        assert_eq!(&ps[..4], &[0, 0, 0, 1]);
        assert_eq!(ps.windows(4).filter(|w| *w == [0, 0, 0, 1]).count(), 2);
    }

    #[test]
    fn adts_frames_keep_header_tail() {
        // Two 48 kHz mono AAC-LC frames of 10 and 12 payload bytes.
        let mut data = Vec::new();
        for len in [10usize, 12] {
            let fl = (len + 7) as u32;
            data.extend_from_slice(&[
                0xFF,
                0xF1,
                0x4C,
                0x40 | ((fl >> 11) & 3) as u8,
                ((fl >> 3) & 0xFF) as u8,
                (((fl & 7) << 5) as u8) | 0x1F,
                0xFC,
            ]);
            data.extend(std::iter::repeat_n(0xAB, len));
        }
        let (frames, ch, rate) = split_adts(&data);
        assert_eq!(frames.len(), 2);
        assert_eq!((ch, rate), (1, 48000));
        assert_eq!(frames[0].len(), 13);
        assert_eq!(frames[0][2], 0xFC);
        assert_eq!(frames[1].len(), 15);
    }

    #[test]
    fn placeholder_is_a_jpeg() {
        assert_eq!(&PLACEHOLDER_JPEG[..2], &[0xFF, 0xD8]);
        assert_eq!(
            &PLACEHOLDER_JPEG[PLACEHOLDER_JPEG.len() - 2..],
            &[0xFF, 0xD9]
        );
    }
}
