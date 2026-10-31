//! The MixStream wire frame.
//!
//! Every binary WebSocket message is:
//!
//! ```text
//! +----------------+----------------------------+-------------------+
//! | u32 big-endian | ASCII header, `k=v&k=v&…`  | payload           |
//! | header length  | (exactly that many bytes)  | (rest of message) |
//! +----------------+----------------------------+-------------------+
//! ```
//!
//! Header keys the reference reads (`ws_bridge.py` `_read_messages`,
//! `_handle_control_frame`, `_dispatch_media_frame`):
//!
//! | key         | meaning |
//! |-------------|---------|
//! | `mediaType` | `1` video, `2` audio |
//! | `msec`      | video position in milliseconds into the recording (history) |
//! | `vdoCodec`  | `H264` / `H265`, on the codec-info frame |
//! | `adoCodec`  | `PCMU`, `MPEG4-GENERIC` (AAC), …, on the codec-info frame |
//! | `adoExtra`  | byte length of the audio-config trailer at the end of the codec-info payload |
//! | `close`     | present on the server's close notice |
//!
//! A frame carrying `close`, `vdoCodec` or `adoCodec` is a control frame
//! (checked before `mediaType`, as the reference does). The codec-info
//! frame's payload holds the SPS/PPS and, on some cameras, the audio
//! trailer `adoExtra` sizes (see [`crate::aac::parse_audio_config`]).
//!
//! The official client logs the same three values as
//! `Data feeder: ContentLen=[%d] MediaType=[%d] Timestamp=[%lld]`
//! (`docs/architecture.md` §5).

use bytes::Bytes;

use crate::error::FrameError;

/// The Annex-B start code prepended to every video payload by [`Frame::annex_b`].
pub const ANNEX_B_START_CODE: [u8; 4] = [0, 0, 0, 1];

/// `mediaType` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaType {
    /// `mediaType=1`.
    Video,
    /// `mediaType=2`.
    Audio,
    /// Any other number.
    Other(u32),
}

/// `vdoCodec` values.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    /// `vdoCodec=H264`.
    H264,
    /// `vdoCodec=H265`.
    H265,
    /// A codec string this crate has not seen.
    Other(String),
}

impl VideoCodec {
    fn parse(s: &str) -> Self {
        match s {
            "H264" => Self::H264,
            "H265" => Self::H265,
            other => Self::Other(other.to_owned()),
        }
    }

    /// The codec name as DSM spells it.
    pub fn as_str(&self) -> &str {
        match self {
            Self::H264 => "H264",
            Self::H265 => "H265",
            Self::Other(s) => s,
        }
    }
}

/// `adoCodec` values.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AudioCodec {
    /// G.711 mu-law, 8 kHz mono.
    Pcmu,
    /// AAC-LC, sent without ADTS framing (see [`crate::aac`]).
    Aac,
    /// A codec string this crate has not seen.
    Other(String),
}

impl AudioCodec {
    fn parse(s: &str) -> Self {
        match s {
            "PCMU" => Self::Pcmu,
            "MPEG4-GENERIC" => Self::Aac,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// The parsed `k=v&…` header of one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    raw: Bytes,
    fields: Vec<(String, String)>,
}

impl Header {
    /// Parse the ASCII header bytes. Pairs keep wire order; a key without
    /// `=` is kept with an empty value; for a repeated key the last value
    /// wins in [`Header::get`] (as a Python dict would).
    pub fn parse(raw: Bytes) -> Self {
        let text = String::from_utf8_lossy(&raw);
        let fields = text
            .split('&')
            .filter(|part| !part.is_empty())
            .map(|part| match part.split_once('=') {
                Some((k, v)) => (k.to_owned(), v.to_owned()),
                None => (part.to_owned(), String::new()),
            })
            .collect();
        Self { raw, fields }
    }

    /// The header exactly as received.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// The last `n` raw header bytes (fewer if the header is shorter).
    ///
    /// One camera model (Reolink RLC-823A in the reference's reports) sends
    /// AAC payloads missing their own leading bytes, which are exactly the
    /// bytes the header *ends* in; the reference passes `header[-4:]` with
    /// every audio frame for that reason (see [`crate::aac::reconstruct_frame`]).
    pub fn tail(&self, n: usize) -> &[u8] {
        let start = self.raw.len().saturating_sub(n);
        &self.raw[start..]
    }

    /// All `(key, value)` pairs in wire order.
    pub fn fields(&self) -> &[(String, String)] {
        &self.fields
    }

    /// The value of `key`, last occurrence wins.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Whether `key` is present at all (with or without a value).
    pub fn contains(&self, key: &str) -> bool {
        self.fields.iter().any(|(k, _)| k == key)
    }

    /// `mediaType`, if present and numeric.
    pub fn media_type(&self) -> Option<MediaType> {
        match self.get("mediaType")?.parse::<u32>().ok()? {
            1 => Some(MediaType::Video),
            2 => Some(MediaType::Audio),
            n => Some(MediaType::Other(n)),
        }
    }

    /// `msec`: milliseconds into the recording (history), if present and numeric.
    pub fn msec(&self) -> Option<u64> {
        self.get("msec")?.parse().ok()
    }

    /// `vdoCodec`, if present.
    pub fn video_codec(&self) -> Option<VideoCodec> {
        self.get("vdoCodec").map(VideoCodec::parse)
    }

    /// `adoCodec`, if present and non-empty.
    pub fn audio_codec(&self) -> Option<AudioCodec> {
        self.get("adoCodec")
            .filter(|s| !s.is_empty())
            .map(AudioCodec::parse)
    }

    /// `adoExtra`: length in bytes of the audio-config trailer, if numeric.
    pub fn audio_extra_len(&self) -> Option<usize> {
        self.get("adoExtra")?.parse().ok()
    }

    /// Whether this is the server's close notice (`close` key present).
    pub fn is_close(&self) -> bool {
        self.contains("close")
    }

    /// Whether this is a control frame: close notice or codec info.
    pub fn is_control(&self) -> bool {
        self.is_close() || self.contains("vdoCodec") || self.contains("adoCodec")
    }
}

/// What a frame is, derived from its header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameKind {
    /// `mediaType=1`: one video access unit without its Annex-B start code.
    Video,
    /// `mediaType=2`: one audio frame (PCMU bytes, or AAC needing [`crate::aac`]).
    Audio,
    /// The server is closing the stream.
    Close,
    /// Codec info, sent first on every (re)connection; payload holds the
    /// parameter sets and optional audio trailer.
    CodecInfo {
        /// `vdoCodec`.
        video: Option<VideoCodec>,
        /// `adoCodec`, if the camera has audio.
        audio: Option<AudioCodec>,
        /// `adoExtra`: trailer length at the end of the payload.
        audio_extra_len: Option<usize>,
    },
    /// No `mediaType` and no control key, or an unknown `mediaType`.
    Unknown,
}

/// One parsed MixStream frame. Owns its bytes (cheaply, via [`Bytes`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    header: Header,
    payload: Bytes,
}

impl Frame {
    /// The parsed header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The payload after the header; may be empty on control frames.
    pub fn payload(&self) -> &Bytes {
        &self.payload
    }

    /// Classify the frame the way the reference's read loop does.
    pub fn kind(&self) -> FrameKind {
        if self.header.is_close() {
            return FrameKind::Close;
        }
        if self.header.is_control() {
            return FrameKind::CodecInfo {
                video: self.header.video_codec(),
                audio: self.header.audio_codec(),
                audio_extra_len: self.header.audio_extra_len(),
            };
        }
        match self.header.media_type() {
            Some(MediaType::Video) => FrameKind::Video,
            Some(MediaType::Audio) => FrameKind::Audio,
            _ => FrameKind::Unknown,
        }
    }

    /// `true` for video and audio frames.
    pub fn is_media(&self) -> bool {
        matches!(self.kind(), FrameKind::Video | FrameKind::Audio)
    }

    /// The payload as an Annex-B access unit: `00 00 00 01` followed by the
    /// payload bytes.
    ///
    /// DSM sends the NAL data without a start code. The reference prepends
    /// exactly one 4-byte start code to the whole payload
    /// (`ws_bridge.py` `_dispatch_media_frame`: "Where DSM leaves it has
    /// never been checked here; the constant is what the black screen
    /// needed"). Whether a payload can hold several NALs that would each
    /// need their own start code is **empirically determined, not
    /// specified**; H.264 and H.265 from several camera models decode with
    /// this. Audio payloads should not be passed through this.
    pub fn annex_b(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(ANNEX_B_START_CODE.len() + self.payload.len());
        out.extend_from_slice(&ANNEX_B_START_CODE);
        out.extend_from_slice(&self.payload);
        out
    }

    /// Split into header and payload.
    pub fn into_parts(self) -> (Header, Bytes) {
        (self.header, self.payload)
    }
}

/// Parse one binary WebSocket message. Copies the bytes; use
/// [`parse_frame_bytes`] when you already hold a [`Bytes`].
pub fn parse_frame(message: &[u8]) -> Result<Frame, FrameError> {
    parse_frame_bytes(Bytes::copy_from_slice(message))
}

/// Parse one binary WebSocket message without copying.
pub fn parse_frame_bytes(message: Bytes) -> Result<Frame, FrameError> {
    if message.len() < 4 {
        return Err(FrameError::TooShort { len: message.len() });
    }
    let declared = u32::from_be_bytes([message[0], message[1], message[2], message[3]]) as usize;
    let available = message.len() - 4;
    if declared > available {
        return Err(FrameError::HeaderTruncated {
            declared,
            available,
        });
    }
    let header = message.slice(4..4 + declared);
    let payload = message.slice(4 + declared..);
    Ok(Frame {
        header: Header::parse(header),
        payload,
    })
}

/// Build a wire frame from header text and payload (the inverse of
/// [`parse_frame`]); used by tests and useful for fixtures.
pub fn encode_frame(header: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + header.len() + payload.len());
    out.extend_from_slice(&(header.len() as u32).to_be_bytes());
    out.extend_from_slice(header);
    out.extend_from_slice(payload);
    out
}

#[cfg(test)]
mod tests {
    //! Fixtures mirror `tests/test_ws_bridge.py` in the reference
    //! (`_frame`, `_codec_frame`, `_aac_audio_frame`,
    //! `_undetectable_prefix_frame`, `_video_frame`,
    //! `_aac_codec_frame_with_config`).
    use super::*;

    fn frame(header: &[u8], payload: &[u8]) -> Frame {
        parse_frame(&encode_frame(header, payload)).unwrap()
    }

    #[test]
    fn video_frame_round_trips_and_gets_a_start_code() {
        let f = frame(b"mediaType=1", &[0; 10]);
        assert_eq!(f.kind(), FrameKind::Video);
        assert_eq!(f.header().media_type(), Some(MediaType::Video));
        assert_eq!(f.payload().len(), 10);
        let au = f.annex_b();
        assert_eq!(&au[..4], &[0, 0, 0, 1]);
        assert_eq!(&au[4..], &[0; 10]);
    }

    #[test]
    fn history_video_frame_exposes_msec() {
        let f = frame(b"mediaType=1&msec=240000", b"AAA");
        assert_eq!(f.kind(), FrameKind::Video);
        assert_eq!(f.header().msec(), Some(240_000));
        assert_eq!(f.payload().as_ref(), b"AAA");
    }

    #[test]
    fn codec_info_video_only() {
        let f = frame(b"vdoCodec=H264", b"");
        assert_eq!(
            f.kind(),
            FrameKind::CodecInfo {
                video: Some(VideoCodec::H264),
                audio: None,
                audio_extra_len: None
            }
        );
        assert!(!f.is_media());
        assert!(f.header().is_control());
    }

    #[test]
    fn codec_info_with_audio_and_trailer() {
        // _aac_codec_frame_with_config(1, 16000): SPS start + "1|16000|" + ASC
        let mut payload = b"\x00\x00\x00\x01\x67\x64\x00\x33".to_vec();
        payload.extend_from_slice(b"1|16000|\x14\x08");
        let f = frame(
            b"vdoCodec=H265&adoCodec=MPEG4-GENERIC&adoExtra=10",
            &payload,
        );
        assert_eq!(
            f.kind(),
            FrameKind::CodecInfo {
                video: Some(VideoCodec::H265),
                audio: Some(AudioCodec::Aac),
                audio_extra_len: Some(10),
            }
        );
        let f = frame(b"vdoCodec=H265&adoCodec=PCMU", b"");
        assert!(matches!(
            f.kind(),
            FrameKind::CodecInfo {
                audio: Some(AudioCodec::Pcmu),
                ..
            }
        ));
        let f = frame(b"vdoCodec=H264&adoCodec=AAC", b"");
        assert!(matches!(
            f.kind(),
            FrameKind::CodecInfo { audio: Some(AudioCodec::Other(ref s)), .. } if s == "AAC"
        ));
    }

    #[test]
    fn close_notice_wins_over_everything() {
        let f = frame(b"close=1&mediaType=1", b"");
        assert_eq!(f.kind(), FrameKind::Close);
        let f = frame(b"close", b"");
        assert_eq!(f.kind(), FrameKind::Close);
    }

    #[test]
    fn audio_frame_header_tail_is_the_last_four_bytes() {
        // _undetectable_prefix_frame(header_suffix=b"&stamp=" + bytes([1, 2, 3, i]))
        let header = [b"mediaType=2&stamp=".as_slice(), &[1, 2, 3, 9]].concat();
        let f = frame(&header, &[0xe0; 50]);
        assert_eq!(f.kind(), FrameKind::Audio);
        assert_eq!(f.header().tail(4), &[1, 2, 3, 9]);
        assert_eq!(f.header().tail(100), header.as_slice());
    }

    #[test]
    fn aac_audio_frame_fixture() {
        // _aac_audio_frame(): 3-byte prefix then the raw block
        let mut payload = vec![0x00, 0x00, 0xe0, 0xaa];
        payload.extend(std::iter::repeat_n(0xaa, 49));
        let f = frame(b"mediaType=2", &payload);
        assert_eq!(f.kind(), FrameKind::Audio);
        assert_eq!(f.payload().len(), 53);
    }

    #[test]
    fn unknown_frames_and_lenient_header_parsing() {
        assert_eq!(frame(b"", b"x").kind(), FrameKind::Unknown);
        assert_eq!(frame(b"mediaType=7", b"x").kind(), FrameKind::Unknown);
        assert_eq!(
            frame(b"mediaType=7", b"x").header().media_type(),
            Some(MediaType::Other(7))
        );
        let h = frame(b"a=1&&b=&a=2&c", b"").into_parts().0;
        assert_eq!(h.get("a"), Some("2"));
        assert_eq!(h.get("b"), Some(""));
        assert_eq!(h.get("c"), Some(""));
        assert_eq!(h.get("d"), None);
        assert_eq!(h.fields().len(), 4);
    }

    #[test]
    fn malformed_messages_are_errors_not_panics() {
        assert_eq!(parse_frame(b""), Err(FrameError::TooShort { len: 0 }));
        assert_eq!(
            parse_frame(b"\x00\x00\x00"),
            Err(FrameError::TooShort { len: 3 })
        );
        assert_eq!(
            parse_frame(b"\x00\x00\x00\x09abc"),
            Err(FrameError::HeaderTruncated {
                declared: 9,
                available: 3
            })
        );
        // Exactly-sized header with no payload is fine.
        let f = parse_frame(b"\x00\x00\x00\x03a=b").unwrap();
        assert_eq!(f.header().get("a"), Some("b"));
        assert!(f.payload().is_empty());
    }

    #[test]
    fn large_frame_is_not_copied_twice() {
        let big = encode_frame(b"mediaType=1", &vec![0xaa; 200 * 1024]);
        let bytes = Bytes::from(big);
        let f = parse_frame_bytes(bytes.clone()).unwrap();
        assert_eq!(f.payload().len(), 200 * 1024);
        assert_eq!(f.payload().as_ptr(), bytes[4 + 11..].as_ptr());
    }
}
