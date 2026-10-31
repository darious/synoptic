//! Typed models for the API responses this crate decodes.
//!
//! Field names follow the reference client's `models.py`; the JSON keys they
//! come from are the ones Surveillance Station actually sends (camelCase,
//! with the occasional `snake_case` or misspelt key). Every numeric field is
//! decoded leniently (see `de.rs`): a missing field is 0 / `""` / `false`,
//! a numeric string is parsed.

use std::fmt;

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::de;

/// A page of a paged list: the items and the server-reported total.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page<T> {
    /// The items of this page.
    pub items: Vec<T>,
    /// The `total` the server reported, or `items.len()` when it sent none.
    pub total: i64,
}

/// A logged-in session.
#[derive(Clone, PartialEq, Eq)]
pub struct Session {
    /// The `_sid` every later request carries.
    pub sid: String,
    /// The trusted-device token (`did`) issued when the login asked for one
    /// with `enable_device_token`; present it as `device_id` on later logins
    /// to skip the OTP prompt.
    pub did: Option<String>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("sid", &"<redacted>")
            .field("did", &self.did.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// One entry of the `SYNO.API.Info` map.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApiInfo {
    /// CGI file name under `/webapi/`, e.g. `entry.cgi` or `auth.cgi`.
    #[serde(default, deserialize_with = "de::string")]
    pub path: String,
    #[serde(default = "one", deserialize_with = "de::int")]
    pub min_version: i64,
    #[serde(default = "one", deserialize_with = "de::int")]
    pub max_version: i64,
}

fn one() -> i64 {
    1
}

/// `Camera.List` v9 `status`. Values other than the three named ones
/// (deleted, unavailable, unauthorised…) land in [`CameraStatus::Other`]
/// and count as not streaming.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CameraStatus {
    Enabled,
    Disconnected,
    Disabled,
    Other(i64),
}

impl CameraStatus {
    /// Synology's number for this status.
    pub fn code(self) -> i64 {
        match self {
            Self::Enabled => 1,
            Self::Disconnected => 3,
            Self::Disabled => 7,
            Self::Other(n) => n,
        }
    }

    /// Only [`CameraStatus::Enabled`] cameras deliver a live stream.
    pub fn is_streaming(self) -> bool {
        self == Self::Enabled
    }
}

impl From<i64> for CameraStatus {
    fn from(n: i64) -> Self {
        match n {
            1 => Self::Enabled,
            3 => Self::Disconnected,
            7 => Self::Disabled,
            other => Self::Other(other),
        }
    }
}

impl<'de> Deserialize<'de> for CameraStatus {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        de::int(d).map(Self::from)
    }
}

/// Which of a camera's Surveillance Station stream profiles a live view
/// asks for. The numbers are Surveillance Station's own, as
/// `Camera.GetInfo`'s `defLiveProfile` reports them and as the MixStream
/// WebSocket URL's `profile=` parameter takes them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StreamProfile {
    High,
    /// Surveillance Station's own default, used when the NAS does not say.
    #[default]
    Balanced,
    Low,
}

impl StreamProfile {
    /// The profile for Surveillance Station's number, if it is one.
    pub fn from_number(n: i64) -> Option<Self> {
        match n {
            0 => Some(Self::High),
            1 => Some(Self::Balanced),
            2 => Some(Self::Low),
            _ => None,
        }
    }

    /// Surveillance Station's number for this profile.
    pub fn number(self) -> u8 {
        match self {
            Self::High => 0,
            Self::Balanced => 1,
            Self::Low => 2,
        }
    }
}

/// A camera from `Camera.List` / `Camera.GetInfo`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(from = "RawCamera")]
pub struct Camera {
    pub id: i64,
    /// `newName` when present, else `name`.
    pub name: String,
    pub model: String,
    pub vendor: String,
    pub status: CameraStatus,
    /// `ptzDirection != 0`.
    pub is_ptz: bool,
    /// The camera has an audio track: `audioCodec` or `audioType` non-zero
    /// (which of the two a DSM version populates varies).
    pub has_audio: bool,
    /// The camera has its own speaker (`audioOut`), for two-way audio.
    pub has_speaker: bool,
    /// The camera's Live View stream profile. Only `Camera.GetInfo` with
    /// `streamInfo=true` reports it; `Camera.List` does not.
    pub def_live_profile: Option<StreamProfile>,
    /// Video codec of the camera's recording/live stream (`videoCodec`).
    pub video_codec: VideoCodec,
}

/// Surveillance Station's `videoCodec` numbers, named as the web UI names
/// them. Best effort: the mapping comes from the web client's constants and
/// is not part of the documented API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    Unknown,
    Mjpeg,
    Mpeg4,
    H264,
    MxPeg,
    H265,
    H264Plus,
    H265Plus,
    Other(i64),
}

impl VideoCodec {
    pub fn from_number(n: i64) -> Self {
        match n {
            0 => Self::Unknown,
            1 => Self::Mjpeg,
            2 => Self::Mpeg4,
            3 => Self::H264,
            4 => Self::MxPeg,
            5 => Self::H265,
            6 => Self::H264Plus,
            7 => Self::H265Plus,
            n => Self::Other(n),
        }
    }

    /// `true` for H.265 and H.265+, the codecs browsers refuse to play.
    pub fn is_hevc(self) -> bool {
        matches!(self, Self::H265 | Self::H265Plus)
    }
}

impl std::fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("-"),
            Self::Mjpeg => f.write_str("MJPEG"),
            Self::Mpeg4 => f.write_str("MPEG-4"),
            Self::H264 => f.write_str("H.264"),
            Self::MxPeg => f.write_str("MxPEG"),
            Self::H265 => f.write_str("H.265"),
            Self::H264Plus => f.write_str("H.264+"),
            Self::H265Plus => f.write_str("H.265+"),
            Self::Other(n) => write!(f, "codec#{n}"),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCamera {
    #[serde(default, deserialize_with = "de::int")]
    id: i64,
    #[serde(default, deserialize_with = "de::string")]
    name: String,
    #[serde(default, deserialize_with = "de::string")]
    new_name: String,
    #[serde(default, deserialize_with = "de::string")]
    model: String,
    #[serde(default, deserialize_with = "de::string")]
    vendor: String,
    #[serde(default, deserialize_with = "de::int")]
    status: i64,
    #[serde(default, deserialize_with = "de::int")]
    ptz_direction: i64,
    #[serde(default, deserialize_with = "de::int")]
    audio_codec: i64,
    #[serde(default, deserialize_with = "de::int")]
    audio_type: i64,
    #[serde(default, deserialize_with = "de::boolish")]
    audio_out: bool,
    #[serde(default, deserialize_with = "de::opt_int")]
    def_live_profile: Option<i64>,
    #[serde(default, deserialize_with = "de::int")]
    video_codec: i64,
}

impl From<RawCamera> for Camera {
    fn from(r: RawCamera) -> Self {
        Self {
            id: r.id,
            name: if r.new_name.is_empty() {
                r.name
            } else {
                r.new_name
            },
            model: r.model,
            vendor: r.vendor,
            status: CameraStatus::from(r.status),
            is_ptz: r.ptz_direction != 0,
            has_audio: r.audio_codec != 0 || r.audio_type != 0,
            has_speaker: r.audio_out,
            def_live_profile: r.def_live_profile.and_then(StreamProfile::from_number),
            video_codec: VideoCodec::from_number(r.video_codec),
        }
    }
}

/// Transport a [`LiveViewPath`] offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LiveProtocol {
    Rtsp,
    RtspOverHttp,
    Mjpeg,
    Multicast,
}

impl LiveProtocol {
    /// The `GetLiveViewPath` response field this protocol is read from.
    pub fn field(self) -> &'static str {
        match self {
            Self::Rtsp => "rtspPath",
            Self::RtspOverHttp => "rtspOverHttpPath",
            Self::Mjpeg => "mjpegHttpPath",
            Self::Multicast => "multicstPath",
        }
    }
}

/// One entry of `Camera.GetLiveViewPath`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct LiveViewPath {
    #[serde(default, deserialize_with = "de::opt_int")]
    pub id: Option<i64>,
    #[serde(default, rename = "rtspPath", deserialize_with = "de::string")]
    pub rtsp_path: String,
    #[serde(default, rename = "rtspOverHttpPath", deserialize_with = "de::string")]
    pub rtsp_over_http_path: String,
    #[serde(default, rename = "mjpegHttpPath", deserialize_with = "de::string")]
    pub mjpeg_http_path: String,
    /// Synology's own spelling of the key is `multicstPath`.
    #[serde(default, rename = "multicstPath", deserialize_with = "de::string")]
    pub multicast_path: String,
}

impl LiveViewPath {
    /// The raw path for `protocol`, `None` when the camera does not offer it.
    pub fn path(&self, protocol: LiveProtocol) -> Option<&str> {
        let p = match protocol {
            LiveProtocol::Rtsp => &self.rtsp_path,
            LiveProtocol::RtspOverHttp => &self.rtsp_over_http_path,
            LiveProtocol::Mjpeg => &self.mjpeg_http_path,
            LiveProtocol::Multicast => &self.multicast_path,
        };
        (!p.is_empty()).then_some(p.as_str())
    }

    /// A playable URL for `protocol`. The MJPEG path is sometimes relative
    /// to the NAS; it is prefixed with `base_url` then.
    pub fn url(&self, protocol: LiveProtocol, base_url: &str) -> Option<String> {
        let p = self.path(protocol)?;
        if protocol == LiveProtocol::Mjpeg
            && !p.starts_with("http://")
            && !p.starts_with("https://")
        {
            return Some(format!("{}{p}", base_url.trim_end_matches('/')));
        }
        Some(p.to_owned())
    }
}

/// `defaultLabel` bit numbers → detection tags.
pub const DETECTION_LABELS: &[(u32, &str)] = &[
    (1, "Person"),
    (2, "Vehicle"),
    (3, "Animal"),
    (4, "Tampering"),
    (6, "Crowd"),
    (7, "License Plate"),
    (8, "Over-occupancy"),
];

/// Decode a `defaultLabel` bitmask into detection tags.
pub fn decode_detection_labels(bitmask: i64) -> Vec<&'static str> {
    DETECTION_LABELS
        .iter()
        .filter(|(bit, _)| bitmask & (1i64 << bit) != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// A recording from `Recording.List`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(from = "RawRecording")]
pub struct Recording {
    pub id: i64,
    pub camera_id: i64,
    /// `camera_name` when present, else `cameraName`.
    pub camera_name: String,
    /// Unix seconds.
    pub start_time: i64,
    pub stop_time: i64,
    pub file_size: i64,
    /// `Recording.List`'s `type`; presumed to be the recording mode
    /// (1 motion, 2 alarm, 3 manual…) and passed on as `recEvtType`.
    pub event_type: i64,
    /// Storage mount / archive the file lives on; opaque, passed straight
    /// through to playback and download calls.
    pub mount_id: i64,
    pub arch_id: i64,
    /// `defaultLabel` bitmask, see [`decode_detection_labels`].
    pub detection_label: i64,
}

impl Recording {
    /// Detection tags from [`Recording::detection_label`].
    pub fn detection_labels(&self) -> Vec<&'static str> {
        decode_detection_labels(self.detection_label)
    }

    /// Recording length in seconds (never negative).
    pub fn duration_secs(&self) -> i64 {
        (self.stop_time - self.start_time).max(0)
    }
}

#[derive(Deserialize)]
struct RawRecording {
    #[serde(default, deserialize_with = "de::int")]
    id: i64,
    #[serde(default, rename = "cameraId", deserialize_with = "de::int")]
    camera_id: i64,
    #[serde(default, deserialize_with = "de::string")]
    camera_name: String,
    #[serde(default, rename = "cameraName", deserialize_with = "de::string")]
    camera_name_camel: String,
    #[serde(default, rename = "startTime", deserialize_with = "de::int")]
    start_time: i64,
    #[serde(default, rename = "stopTime", deserialize_with = "de::int")]
    stop_time: i64,
    #[serde(default, rename = "fileSize", deserialize_with = "de::int")]
    file_size: i64,
    #[serde(default, rename = "type", deserialize_with = "de::int")]
    event_type: i64,
    #[serde(default, rename = "mountId", deserialize_with = "de::int")]
    mount_id: i64,
    #[serde(default, rename = "archId", deserialize_with = "de::int")]
    arch_id: i64,
    #[serde(default, rename = "defaultLabel", deserialize_with = "de::int")]
    detection_label: i64,
}

impl From<RawRecording> for Recording {
    fn from(r: RawRecording) -> Self {
        Self {
            id: r.id,
            camera_id: r.camera_id,
            camera_name: if r.camera_name.is_empty() {
                r.camera_name_camel
            } else {
                r.camera_name
            },
            start_time: r.start_time,
            stop_time: r.stop_time,
            file_size: r.file_size,
            event_type: r.event_type,
            mount_id: r.mount_id,
            arch_id: r.arch_id,
            detection_label: r.detection_label,
        }
    }
}

/// A saved snapshot from `SnapShot.List`.
///
/// The list identifies the camera by name only; `camera_id` is not in the
/// response and stays 0 unless the caller fills it in from the camera list.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(from = "RawSnapshot")]
pub struct Snapshot {
    pub id: i64,
    pub camera_name: String,
    /// `createdTm`, falling back to `displayTm`.
    pub create_time: i64,
    pub camera_id: i64,
    /// Base64 JPEG thumbnail included inline in the list response.
    pub image_data: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSnapshot {
    #[serde(default, deserialize_with = "de::int")]
    id: i64,
    #[serde(default, deserialize_with = "de::string")]
    cam_name: String,
    #[serde(default, deserialize_with = "de::opt_int")]
    created_tm: Option<i64>,
    #[serde(default, deserialize_with = "de::int")]
    display_tm: i64,
    #[serde(default, deserialize_with = "de::string")]
    image_data: String,
}

impl From<RawSnapshot> for Snapshot {
    fn from(r: RawSnapshot) -> Self {
        Self {
            id: r.id,
            camera_name: r.cam_name,
            create_time: r.created_tm.unwrap_or(r.display_tm),
            camera_id: 0,
            image_data: r.image_data,
        }
    }
}

/// A surveillance event as `models.py` shapes it (the fields common to the
/// recording-based event sources).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(from = "RawEvent")]
pub struct Event {
    pub id: i64,
    pub camera_id: i64,
    pub camera_name: String,
    /// `mode`, else `type`, else `eventType` (1 motion, 2 alarm, 3 manual…).
    pub event_type: i64,
    pub start_time: i64,
    pub stop_time: i64,
    pub mount_id: i64,
    pub arch_id: i64,
    pub detection_label: i64,
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(default, deserialize_with = "de::int")]
    id: i64,
    #[serde(default, rename = "cameraId", deserialize_with = "de::int")]
    camera_id: i64,
    #[serde(default, deserialize_with = "de::string")]
    camera_name: String,
    #[serde(default, rename = "cameraName", deserialize_with = "de::string")]
    camera_name_camel: String,
    #[serde(default, deserialize_with = "de::opt_int")]
    mode: Option<i64>,
    #[serde(default, rename = "type", deserialize_with = "de::opt_int")]
    kind: Option<i64>,
    #[serde(default, rename = "eventType", deserialize_with = "de::int")]
    event_type: i64,
    #[serde(default, rename = "startTime", deserialize_with = "de::int")]
    start_time: i64,
    #[serde(default, rename = "stopTime", deserialize_with = "de::int")]
    stop_time: i64,
    #[serde(default, rename = "mountId", deserialize_with = "de::int")]
    mount_id: i64,
    #[serde(default, rename = "archId", deserialize_with = "de::int")]
    arch_id: i64,
    #[serde(default, rename = "defaultLabel", deserialize_with = "de::int")]
    detection_label: i64,
}

impl From<RawEvent> for Event {
    fn from(r: RawEvent) -> Self {
        Self {
            id: r.id,
            camera_id: r.camera_id,
            camera_name: if r.camera_name.is_empty() {
                r.camera_name_camel
            } else {
                r.camera_name
            },
            event_type: r.mode.or(r.kind).unwrap_or(r.event_type),
            start_time: r.start_time,
            stop_time: r.stop_time,
            mount_id: r.mount_id,
            arch_id: r.arch_id,
            detection_label: r.detection_label,
        }
    }
}

/// One entry of `EventCenter.Event.List` (Surveillance Station 9.x's typed
/// event log). `duration` is 0 while an event is still going on.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct EventCenterEvent {
    #[serde(default, deserialize_with = "de::int")]
    pub id: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub camera_id: i64,
    /// Unix seconds.
    #[serde(default, deserialize_with = "de::int")]
    pub start_time: i64,
    /// Seconds.
    #[serde(default, deserialize_with = "de::int")]
    pub duration: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub event_type: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub object_type: i64,
}

/// `EventCenter.Event.Capability`: the event and object types this NAS can
/// report, as comma-separated numbers.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct EventCenterCapability {
    #[serde(default, deserialize_with = "de::csv_ints")]
    pub event_types: Vec<i64>,
    #[serde(default, deserialize_with = "de::csv_ints")]
    pub object_types: Vec<i64>,
}

/// One recording file in a `RecordingPicker.EnumInterval` answer.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct IntervalRecording {
    #[serde(default, deserialize_with = "de::int")]
    pub id: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub start: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub stop: i64,
}

/// One camera's part of a `RecordingPicker.EnumInterval` answer.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct IntervalCamera {
    #[serde(default, deserialize_with = "de::int")]
    pub camera_id: i64,
    #[serde(default, rename = "mountId", deserialize_with = "de::int")]
    pub mount_id: i64,
    #[serde(default, rename = "archId", deserialize_with = "de::int")]
    pub arch_id: i64,
    /// The recording files in range (`event`).
    #[serde(default, rename = "event")]
    pub recordings: Vec<IntervalRecording>,
    /// The per-bucket `event_map`, left raw: its bit layout is the legacy
    /// event backend's business.
    #[serde(default)]
    pub event_map: Value,
}

impl IntervalCamera {
    /// Merged, sorted `(start, stop)` spans where a recording exists.
    pub fn presence_spans(&self) -> Vec<(i64, i64)> {
        merge_intervals(self.recordings.iter().map(|r| (r.start, r.stop)))
    }
}

/// Sort spans and merge every overlapping/adjacent pair.
pub fn merge_intervals(spans: impl IntoIterator<Item = (i64, i64)>) -> Vec<(i64, i64)> {
    let mut spans: Vec<_> = spans.into_iter().collect();
    spans.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::with_capacity(spans.len());
    for (start, stop) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(stop),
            _ => merged.push((start, stop)),
        }
    }
    merged
}

/// An alert from `Notification.List`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    #[serde(default, deserialize_with = "de::int")]
    pub id: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub camera_id: i64,
    #[serde(default, deserialize_with = "de::string")]
    pub camera_name: String,
    #[serde(default, deserialize_with = "de::int")]
    pub alert_type: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub timestamp: i64,
    #[serde(default, deserialize_with = "de::boolish")]
    pub is_read: bool,
}

/// `HomeMode.GetInfo`.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct HomeModeInfo {
    #[serde(default, deserialize_with = "de::boolish")]
    pub on: bool,
}

/// A PTZ preset position.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct PtzPreset {
    #[serde(default, deserialize_with = "de::int")]
    pub id: i64,
    #[serde(default, deserialize_with = "de::string")]
    pub name: String,
    #[serde(default, deserialize_with = "de::int")]
    pub position: i64,
}

/// A PTZ patrol route (the route itself lives on the NAS).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct PtzPatrol {
    #[serde(default, deserialize_with = "de::int")]
    pub id: i64,
    #[serde(default, deserialize_with = "de::string")]
    pub name: String,
}

/// One camera license.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct License {
    #[serde(default, deserialize_with = "de::int")]
    pub id: i64,
    #[serde(default, deserialize_with = "de::string")]
    pub key: String,
    #[serde(default, deserialize_with = "de::int")]
    pub quota: i64,
    /// Unix seconds, 0 = never expires.
    #[serde(default, deserialize_with = "de::int")]
    pub expired_date: i64,
    #[serde(default, rename = "isExpired", deserialize_with = "de::boolish")]
    pub is_expired: bool,
    #[serde(default, rename = "isMigrated", deserialize_with = "de::boolish")]
    pub is_migrated: bool,
}

/// `License.Load`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct LicenseInfo {
    #[serde(default, deserialize_with = "de::int")]
    pub key_max: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub key_total: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub key_used: i64,
    #[serde(default, rename = "license")]
    pub licenses: Vec<License>,
}

/// A time lapse task.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TimeLapseTask {
    #[serde(default, deserialize_with = "de::int")]
    pub id: i64,
    #[serde(default, deserialize_with = "de::string")]
    pub name: String,
    #[serde(default, deserialize_with = "de::int")]
    pub camera_id: i64,
    #[serde(default, deserialize_with = "de::string")]
    pub camera_name: String,
}

/// A time lapse recording segment.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(from = "RawTimeLapseRecording")]
pub struct TimeLapseRecording {
    pub id: i64,
    pub camera_id: i64,
    pub camera_name: String,
    pub start_time: i64,
    pub stop_time: i64,
    /// `event_size_bytes`.
    pub file_size: i64,
    pub mount_id: i64,
    pub arch_id: i64,
    /// `status_flags` bit 2.
    pub is_locked: bool,
    pub recording: bool,
    pub path: String,
}

#[derive(Deserialize)]
struct RawTimeLapseRecording {
    #[serde(default, deserialize_with = "de::int")]
    id: i64,
    #[serde(default, rename = "cameraId", deserialize_with = "de::int")]
    camera_id: i64,
    #[serde(default, deserialize_with = "de::string")]
    camera_name: String,
    #[serde(default, rename = "cameraName", deserialize_with = "de::string")]
    camera_name_camel: String,
    #[serde(default, rename = "startTime", deserialize_with = "de::int")]
    start_time: i64,
    #[serde(default, rename = "stopTime", deserialize_with = "de::int")]
    stop_time: i64,
    #[serde(default, deserialize_with = "de::int")]
    event_size_bytes: i64,
    #[serde(default, rename = "mountId", deserialize_with = "de::int")]
    mount_id: i64,
    #[serde(default, rename = "archId", deserialize_with = "de::int")]
    arch_id: i64,
    #[serde(default, deserialize_with = "de::int")]
    status_flags: i64,
    #[serde(default, deserialize_with = "de::boolish")]
    recording: bool,
    #[serde(default, deserialize_with = "de::string")]
    path: String,
}

impl From<RawTimeLapseRecording> for TimeLapseRecording {
    fn from(r: RawTimeLapseRecording) -> Self {
        Self {
            id: r.id,
            camera_id: r.camera_id,
            camera_name: if r.camera_name.is_empty() {
                r.camera_name_camel
            } else {
                r.camera_name
            },
            start_time: r.start_time,
            stop_time: r.stop_time,
            file_size: r.event_size_bytes,
            mount_id: r.mount_id,
            arch_id: r.arch_id,
            is_locked: r.status_flags & 4 != 0,
            recording: r.recording,
            path: r.path,
        }
    }
}

/// Surveillance Station's version as `Info.GetInfo` reports it (each part
/// arrives as a string on most DSM versions).
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct SsVersion {
    #[serde(default, deserialize_with = "de::int")]
    pub major: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub minor: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub small: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub build: i64,
}

impl fmt::Display for SsVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}-{}",
            self.major, self.minor, self.small, self.build
        )
    }
}

/// `SYNO.SurveillanceStation.Info` `GetInfo`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SsInfo {
    /// Package version; absent on very old releases.
    #[serde(default)]
    pub version: Option<SsVersion>,
    /// NAS serial number (version 2 and later).
    #[serde(default, deserialize_with = "de::string")]
    pub serial: String,
    /// NAS model, as the official app reads it.
    #[serde(default, rename = "DSModelName", deserialize_with = "de::string")]
    pub ds_model_name: String,
    #[serde(default, deserialize_with = "de::string")]
    pub hostname: String,
    #[serde(default, deserialize_with = "de::int")]
    pub camera_number: i64,
    /// Synology's own key is misspelt `liscenseNumber`.
    #[serde(default, rename = "liscenseNumber", deserialize_with = "de::int")]
    pub license_number: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub max_camera_support: i64,
    #[serde(default, deserialize_with = "de::boolish")]
    pub is_license_enough: bool,
    #[serde(default, deserialize_with = "de::int")]
    pub customized_port_http: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub customized_port_https: i64,
    /// Web UI path, e.g. `/webman/3rdparty/SurveillanceStation/`.
    #[serde(default, deserialize_with = "de::string")]
    pub path: String,
    #[serde(default, deserialize_with = "de::string")]
    pub timezone: String,
    #[serde(default, rename = "timezoneTZDB", deserialize_with = "de::string")]
    pub timezone_tzdb: String,
    #[serde(default, deserialize_with = "de::int")]
    pub uid: i64,
    #[serde(default, deserialize_with = "de::int")]
    pub user_priv: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn camera_basic() {
        let cam: Camera = serde_json::from_value(json!({
            "id": 1, "newName": "Front Door", "ip": "192.168.1.50", "port": 554,
            "model": "DS-2CD2386G2", "vendor": "Hikvision", "status": 1,
            "ptzDirection": 1, "resolution": "3840x2160", "fps": 25,
            "audioType": 2, "audioOut": true
        }))
        .unwrap();
        assert_eq!(cam.id, 1);
        assert_eq!(cam.name, "Front Door");
        assert_eq!(cam.status, CameraStatus::Enabled);
        assert!(cam.is_ptz && cam.has_audio && cam.has_speaker);
        assert_eq!(cam.vendor, "Hikvision");
        assert_eq!(cam.def_live_profile, None);
    }

    #[test]
    fn camera_fallbacks_and_statuses() {
        let cam: Camera =
            serde_json::from_value(json!({"id": "2", "name": "Back Yard", "status": "7"})).unwrap();
        assert_eq!(cam.id, 2);
        assert_eq!(cam.name, "Back Yard");
        assert_eq!(cam.status, CameraStatus::Disabled);

        let statuses: Vec<CameraStatus> = [1, 2, 3, 7]
            .iter()
            .map(|n| {
                serde_json::from_value::<Camera>(json!({"status": n}))
                    .unwrap()
                    .status
            })
            .collect();
        assert_eq!(statuses[0], CameraStatus::Enabled);
        assert_eq!(statuses[1], CameraStatus::Other(2));
        assert_eq!(statuses[2], CameraStatus::Disconnected);
        assert_eq!(statuses[3], CameraStatus::Disabled);
        assert!(!statuses[1].is_streaming());

        let cam: Camera = serde_json::from_value(json!({})).unwrap();
        assert_eq!(cam.id, 0);
        assert_eq!(cam.name, "");
        assert_eq!(cam.status, CameraStatus::Other(0));
        assert!(!cam.is_ptz && !cam.has_audio && !cam.has_speaker);

        let cam: Camera =
            serde_json::from_value(json!({"audioType": 0, "audioOut": false, "audioCodec": 0}))
                .unwrap();
        assert!(!cam.has_audio && !cam.has_speaker);
        let cam: Camera = serde_json::from_value(json!({"audioCodec": 4})).unwrap();
        assert!(cam.has_audio);
    }

    #[test]
    fn camera_def_live_profile() {
        let cam: Camera = serde_json::from_value(json!({"id": 3, "defLiveProfile": "2"})).unwrap();
        assert_eq!(cam.def_live_profile, Some(StreamProfile::Low));
        let cam: Camera = serde_json::from_value(json!({"id": 3, "defLiveProfile": 9})).unwrap();
        assert_eq!(cam.def_live_profile, None);
    }

    #[test]
    fn recording() {
        let rec: Recording = serde_json::from_value(json!({
            "id": 100, "cameraId": 1, "cameraName": "Front Door",
            "startTime": 1_700_000_000, "stopTime": 1_700_003_600,
            "type": "motion", "mountId": 0, "defaultLabel": 6
        }))
        .unwrap();
        assert_eq!(rec.id, 100);
        assert_eq!(rec.camera_id, 1);
        assert_eq!(rec.camera_name, "Front Door");
        assert_eq!(rec.start_time, 1_700_000_000);
        assert_eq!(rec.stop_time, 1_700_003_600);
        assert_eq!(rec.event_type, 0);
        assert_eq!(rec.duration_secs(), 3600);
        assert_eq!(rec.detection_labels(), vec!["Person", "Vehicle"]);

        let rec: Recording =
            serde_json::from_value(json!({"camera_name": "A", "cameraName": "B"})).unwrap();
        assert_eq!(rec.camera_name, "A");
    }

    #[test]
    fn snapshot() {
        let snap: Snapshot = serde_json::from_value(json!({
            "id": 50, "camName": "Garage", "createdTm": 1_700_001_000, "imageData": "Zm9v"
        }))
        .unwrap();
        assert_eq!(snap.id, 50);
        assert_eq!(snap.camera_id, 0);
        assert_eq!(snap.camera_name, "Garage");
        assert_eq!(snap.create_time, 1_700_001_000);
        assert_eq!(snap.image_data, "Zm9v");

        let snap: Snapshot = serde_json::from_value(
            json!({"id": 51, "camName": "Yard", "displayTm": 1_700_002_000}),
        )
        .unwrap();
        assert_eq!(snap.create_time, 1_700_002_000);
    }

    #[test]
    fn event() {
        let ev: Event = serde_json::from_value(json!({
            "id": 200, "cameraId": 1, "cameraName": "Front Door", "eventType": 1,
            "startTime": 1_700_000_000, "stopTime": 1_700_000_060, "reason": "Motion detected"
        }))
        .unwrap();
        assert_eq!(ev.id, 200);
        assert_eq!(ev.event_type, 1);
        let ev: Event =
            serde_json::from_value(json!({"mode": 3, "type": 2, "eventType": 1})).unwrap();
        assert_eq!(ev.event_type, 3);
        let ev: Event = serde_json::from_value(json!({"type": 2, "eventType": 1})).unwrap();
        assert_eq!(ev.event_type, 2);
    }

    #[test]
    fn alert_and_home_mode() {
        let alert: Alert = serde_json::from_value(json!({
            "id": 300, "cameraId": 1, "cameraName": "Front Door", "alertType": 0,
            "timestamp": 1_700_000_000, "isRead": false
        }))
        .unwrap();
        assert_eq!(alert.id, 300);
        assert!(!alert.is_read);
        let hm: HomeModeInfo = serde_json::from_value(json!({"on": true})).unwrap();
        assert!(hm.on);
        let hm: HomeModeInfo = serde_json::from_value(json!({})).unwrap();
        assert!(!hm.on);
    }

    #[test]
    fn ptz_models() {
        let p: PtzPreset =
            serde_json::from_value(json!({"id": 1, "name": "Home", "position": 0})).unwrap();
        assert_eq!((p.id, p.name.as_str()), (1, "Home"));
        let p: PtzPatrol = serde_json::from_value(json!({"id": 1, "name": "Perimeter"})).unwrap();
        assert_eq!((p.id, p.name.as_str()), (1, "Perimeter"));
    }

    #[test]
    fn api_info() {
        let info: ApiInfo =
            serde_json::from_value(json!({"path": "entry.cgi", "minVersion": 1, "maxVersion": 9}))
                .unwrap();
        assert_eq!(info.path, "entry.cgi");
        assert_eq!(info.max_version, 9);
        let info: ApiInfo = serde_json::from_value(json!({})).unwrap();
        assert_eq!(info.max_version, 1);
    }

    #[test]
    fn licenses() {
        let lic: License = serde_json::from_value(json!({
            "id": 1, "key": "ABCD-1234-EFGH-5678", "quota": 2, "expired_date": 1_735_689_600,
            "isExpired": false, "isMigrated": false, "ownerDsId": 42
        }))
        .unwrap();
        assert_eq!(lic.key, "ABCD-1234-EFGH-5678");
        assert_eq!(lic.quota, 2);
        let lic: License = serde_json::from_value(json!({})).unwrap();
        assert_eq!(lic, License::default());

        let info: LicenseInfo = serde_json::from_value(json!({
            "key_max": 8, "key_total": 4, "key_used": 3,
            "license": [
                {"id": 1, "key": "AAAA", "quota": 1, "expired_date": 0},
                {"id": 2, "key": "EEEE", "quota": 2, "expired_date": 1, "isExpired": true}
            ]
        }))
        .unwrap();
        assert_eq!((info.key_max, info.key_total, info.key_used), (8, 4, 3));
        assert_eq!(info.licenses.len(), 2);
        assert!(info.licenses[1].is_expired);
    }

    #[test]
    fn time_lapse() {
        let task: TimeLapseTask = serde_json::from_value(json!({
            "id": 1, "name": "Front Door Lapse", "cameraId": 3, "cameraName": "Front Door",
            "enabled": true, "status": 2
        }))
        .unwrap();
        assert_eq!(task.camera_id, 3);
        let rec: TimeLapseRecording = serde_json::from_value(json!({
            "id": 10, "cameraId": 3, "camera_name": "Front Door", "startTime": 1, "stopTime": 2,
            "event_size_bytes": 5_242_880, "status_flags": 0, "recording": false,
            "path": "/volume1/surveillance/timelapse/1.mp4"
        }))
        .unwrap();
        assert_eq!(rec.file_size, 5_242_880);
        assert!(!rec.is_locked);
        let rec: TimeLapseRecording =
            serde_json::from_value(json!({"cameraName": "Garage", "status_flags": 4})).unwrap();
        assert!(rec.is_locked);
        assert_eq!(rec.camera_name, "Garage");
    }

    #[test]
    fn live_view_path_selection() {
        let p: LiveViewPath = serde_json::from_value(json!({
            "id": 1,
            "rtspPath": "rtsp://nas:554/Sms=1.unicast",
            "rtspOverHttpPath": "http://nas:5000/webapi/...",
            "mjpegHttpPath": "/webapi/entry.cgi?api=SYNO.SurveillanceStation.Stream",
            "multicstPath": ""
        }))
        .unwrap();
        assert_eq!(
            p.url(LiveProtocol::Rtsp, "https://nas:5001").as_deref(),
            Some("rtsp://nas:554/Sms=1.unicast")
        );
        assert_eq!(
            p.url(LiveProtocol::Mjpeg, "https://nas:5001/").as_deref(),
            Some("https://nas:5001/webapi/entry.cgi?api=SYNO.SurveillanceStation.Stream")
        );
        assert_eq!(p.url(LiveProtocol::Multicast, "https://nas:5001"), None);
        assert_eq!(LiveProtocol::Multicast.field(), "multicstPath");
    }

    #[test]
    fn ss_info_with_string_numbers() {
        let info: SsInfo = serde_json::from_value(json!({
            "version": {"major": "9", "minor": "2", "small": "0", "build": "11289"},
            "serial": "1234ABC", "DSModelName": "DS920+", "cameraNumber": "3",
            "liscenseNumber": 2, "isLicenseEnough": 1
        }))
        .unwrap();
        assert_eq!(info.version.as_ref().unwrap().to_string(), "9.2.0-11289");
        assert_eq!(info.ds_model_name, "DS920+");
        assert_eq!(info.camera_number, 3);
        assert_eq!(info.license_number, 2);
        assert!(info.is_license_enough);
    }

    #[test]
    fn intervals_merge() {
        assert_eq!(
            merge_intervals([(10, 20), (0, 5), (5, 8), (19, 25)]),
            vec![(0, 8), (10, 25)]
        );
        let cam: IntervalCamera = serde_json::from_value(json!({
            "camera_id": 4, "mountId": 0, "archId": 1,
            "event": [{"id": 1, "start": 0, "stop": 10}, {"id": 2, "start": 10, "stop": 20}],
            "event_map": [[1, 0, 0]]
        }))
        .unwrap();
        assert_eq!(cam.presence_spans(), vec![(0, 20)]);
        assert_eq!(cam.arch_id, 1);
    }

    #[test]
    fn capability_csv() {
        let cap: EventCenterCapability =
            serde_json::from_value(json!({"event_types": "1,2,5", "object_types": "1,2"})).unwrap();
        assert_eq!(cap.event_types, vec![1, 2, 5]);
        assert_eq!(cap.object_types, vec![1, 2]);
    }

    #[test]
    fn session_debug_redacts() {
        let s = Session {
            sid: "topsecret".into(),
            did: Some("device".into()),
        };
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("topsecret") && !dbg.contains("device"));
    }
}
