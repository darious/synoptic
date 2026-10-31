//! The `config.toml` data model.
//!
//! Loading is deliberately lenient, exactly where the reference is: a
//! hand-edited value of the wrong type falls back to its default (or is
//! dropped from a camera table) instead of refusing the whole file, and
//! keys this crate does not know are kept in the `extra` table of the
//! section they were found in and written back unchanged.
//!
//! Saving writes what the reference writes: every `[general]` and
//! `[session]` key, the three `setting_overrides*` tables (even empty),
//! and each profile with only its non-empty camera tables.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use toml::{Table, Value};

use crate::Paths;

/// Floor for the `poll_interval_*` settings (seconds). The reference
/// feeds them to a timer where 0 means "as fast as possible".
pub const MIN_POLL_INTERVAL: u32 = 5;

/// Default `port` of a connection profile (DSM HTTPS).
pub const DEFAULT_PORT: u16 = 5001;

/// The grid layouts the reference offers; `layout_cameras` is keyed by these.
pub const GRID_LAYOUTS: &[&str] = &["1x1", "2x2", "3x3", "4x4"];

/// Per-camera stream protocols accepted in `camera_protocols`.
/// `auto` is the same as `websocket`; `direct` plays `camera_overrides`.
pub const STREAM_PROTOCOLS: &[&str] = &[
    "auto",
    "websocket",
    "mjpeg",
    "rtsp_over_http",
    "rtsp",
    "multicast",
    "direct",
];

/// Fisheye dewarp modes accepted in `camera_dewarp`.
pub const DEWARP_MODES: &[&str] = &["panorama", "ptz"];

/// Fisheye mount accepted in `camera_fisheye_mount` (absent = ceiling).
pub const FISHEYE_MOUNT_WALL: &str = "wall";

// ---------------------------------------------------------------------------
// Small value enums
// ---------------------------------------------------------------------------

/// `[general] theme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the desktop.
    #[default]
    Auto,
    Dark,
    Light,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Auto => "auto",
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }
}

impl FromStr for Theme {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "auto" => Ok(Theme::Auto),
            "dark" => Ok(Theme::Dark),
            "light" => Ok(Theme::Light),
            _ => Err(()),
        }
    }
}

impl fmt::Display for Theme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which of a camera's Surveillance Station stream profiles Live View
/// asks for (`camera_live_view_stream_profiles` and the
/// `live_view_stream_profile` Settings-page dropdown).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamProfile {
    /// Follow the camera's own Live View setting on the NAS.
    #[default]
    Camera,
    High,
    Balanced,
    Low,
}

impl StreamProfile {
    /// In the reference's display order, with its labels.
    pub const ALL: &'static [(StreamProfile, &'static str)] = &[
        (StreamProfile::Camera, "Use camera settings"),
        (StreamProfile::High, "High quality"),
        (StreamProfile::Balanced, "Balanced"),
        (StreamProfile::Low, "Low bandwidth"),
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            StreamProfile::Camera => "camera",
            StreamProfile::High => "high",
            StreamProfile::Balanced => "balanced",
            StreamProfile::Low => "low",
        }
    }
}

impl FromStr for StreamProfile {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "camera" => Ok(StreamProfile::Camera),
            "high" => Ok(StreamProfile::High),
            "balanced" => Ok(StreamProfile::Balanced),
            "low" => Ok(StreamProfile::Low),
            _ => Err(()),
        }
    }
}

impl fmt::Display for StreamProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Quick time ranges of the search pages (`*_search_time_preset`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimePreset {
    Today,
    Yesterday,
    Last24h,
    Last7d,
    Last30d,
}

impl TimePreset {
    pub const ALL: &'static [(TimePreset, &'static str)] = &[
        (TimePreset::Today, "Today"),
        (TimePreset::Yesterday, "Yesterday"),
        (TimePreset::Last24h, "Last 24 hrs"),
        (TimePreset::Last7d, "Last 7 days"),
        (TimePreset::Last30d, "Last 30 days"),
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TimePreset::Today => "today",
            TimePreset::Yesterday => "yesterday",
            TimePreset::Last24h => "last24h",
            TimePreset::Last7d => "last7d",
            TimePreset::Last30d => "last30d",
        }
    }

    /// `None` for the empty string and, as the reference does, for any
    /// preset it does not know (a typo made its pages hang).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "today" => Some(TimePreset::Today),
            "yesterday" => Some(TimePreset::Yesterday),
            "last24h" => Some(TimePreset::Last24h),
            "last7d" => Some(TimePreset::Last7d),
            "last30d" => Some(TimePreset::Last30d),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// [general]
// ---------------------------------------------------------------------------

/// The `[general]` table.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct General {
    /// Name of the profile logged into at startup; `""` until one is added.
    pub default_profile: String,
    /// Seconds between camera-list polls (also the lost-stream retry).
    pub poll_interval_cameras: u32,
    pub poll_interval_alerts: u32,
    pub poll_interval_homemode: u32,
    /// Seconds between Transactions (POS receipt) polls.
    pub poll_interval_transactions: u32,
    pub theme: Theme,
    pub sidebar_visible: bool,
    pub timeline_visible: bool,
    /// Release tag whose "new release" notice the user has dismissed.
    pub dismissed_update_version: String,
    /// Folder the Save dialog opens in; defaults to `<data_dir>/snapshots`.
    pub snapshot_dir: String,
    /// Keys this crate does not know, written back unchanged.
    #[serde(flatten)]
    pub extra: Table,
}

impl Default for General {
    fn default() -> Self {
        Self {
            default_profile: String::new(),
            poll_interval_cameras: 30,
            poll_interval_alerts: 30,
            poll_interval_homemode: 60,
            poll_interval_transactions: 5,
            theme: Theme::Auto,
            sidebar_visible: true,
            timeline_visible: true,
            dismissed_update_version: String::new(),
            snapshot_dir: default_snapshot_dir(),
            extra: Table::new(),
        }
    }
}

/// What the reference puts in `snapshot_dir` when the file has none.
pub fn default_snapshot_dir() -> String {
    Paths::from_env()
        .map(|p| p.default_snapshot_dir().to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl General {
    fn from_table(mut t: Table) -> Self {
        let d = Self::default();
        let theme = match t.remove("theme") {
            Some(Value::String(s)) if s.parse::<Theme>().is_ok() => s.parse().unwrap_or_default(),
            _ => match t.remove("dark_theme") {
                // Migrated from the old `dark_theme` boolean; dropped on save
                // like the reference does.
                Some(Value::Boolean(true)) => Theme::Dark,
                Some(Value::Boolean(false)) => Theme::Light,
                _ => Theme::Auto,
            },
        };
        Self {
            default_profile: str_or(t.remove("default_profile"), &d.default_profile),
            poll_interval_cameras: poll_interval(&mut t, "poll_interval_cameras", 30),
            poll_interval_alerts: poll_interval(&mut t, "poll_interval_alerts", 30),
            poll_interval_homemode: poll_interval(&mut t, "poll_interval_homemode", 60),
            poll_interval_transactions: poll_interval(&mut t, "poll_interval_transactions", 5),
            theme,
            sidebar_visible: bool_or(t.remove("sidebar_visible"), true),
            timeline_visible: bool_or(t.remove("timeline_visible"), true),
            dismissed_update_version: str_or(t.remove("dismissed_update_version"), ""),
            snapshot_dir: str_or(t.remove("snapshot_dir"), &d.snapshot_dir),
            extra: t,
        }
    }
}

fn poll_interval(t: &mut Table, key: &str, default: u32) -> u32 {
    let Some(v) = t.remove(key) else {
        return default;
    };
    let Some(n) = int_of(&v) else {
        tracing::warn!("Config: {key} is not a number, using {default}s");
        return default;
    };
    if n < i64::from(MIN_POLL_INTERVAL) {
        tracing::warn!("Config: {key} of {n}s is below the {MIN_POLL_INTERVAL}s minimum");
        return MIN_POLL_INTERVAL;
    }
    u32::try_from(n).unwrap_or(u32::MAX)
}

// ---------------------------------------------------------------------------
// [session]
// ---------------------------------------------------------------------------

/// The `[session]` table: what the UI restores on the next start.
///
/// Search times are ISO-8601 text as the reference writes it
/// (`2026-02-01T00:00:00`, local time, no zone); an unquoted TOML
/// datetime is accepted and turned into that text.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Session {
    /// `"1x1"`, `"2x2"`, `"3x3"` or `"4x4"`.
    pub grid_layout: String,
    /// Last active page, e.g. `"live"`.
    pub last_page: String,
    pub search_from_time: String,
    pub search_to_time: String,
    /// A [`TimePreset`] name or `""`.
    pub search_time_preset: String,
    pub events_search_from_time: String,
    pub events_search_to_time: String,
    /// Events defaults to `"today"`; its pages stop at `"last24h"`.
    pub events_search_time_preset: String,
    /// Event filter keys such as `"08"` or `"25:hikvision"`; an older
    /// file's integer entries are dropped.
    pub events_search_event_types: Vec<String>,
    /// `false` = any of the types (OR), `true` = all of them (AND).
    pub events_search_event_types_match_all: bool,
    pub snapshots_search_from_time: String,
    pub snapshots_search_to_time: String,
    pub snapshots_search_time_preset: String,
    /// Keys this crate does not know, written back unchanged.
    #[serde(flatten)]
    pub extra: Table,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            grid_layout: "2x2".into(),
            last_page: "live".into(),
            search_from_time: String::new(),
            search_to_time: String::new(),
            search_time_preset: String::new(),
            events_search_from_time: String::new(),
            events_search_to_time: String::new(),
            events_search_time_preset: "today".into(),
            events_search_event_types: Vec::new(),
            events_search_event_types_match_all: false,
            snapshots_search_from_time: String::new(),
            snapshots_search_to_time: String::new(),
            snapshots_search_time_preset: String::new(),
            extra: Table::new(),
        }
    }
}

impl Session {
    fn from_table(mut t: Table, old_grid_layout: Option<Value>) -> Self {
        let grid_layout = match t.remove("grid_layout") {
            Some(v) => str_or(Some(v), "2x2"),
            None => str_or(old_grid_layout, "2x2"),
        };
        let event_types = match t.remove("events_search_event_types") {
            Some(Value::Array(items)) => items
                .into_iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        Self {
            grid_layout,
            last_page: str_or(t.remove("last_page"), "live"),
            search_from_time: time_text(t.remove("search_from_time")),
            search_to_time: time_text(t.remove("search_to_time")),
            search_time_preset: str_or(t.remove("search_time_preset"), ""),
            events_search_from_time: time_text(t.remove("events_search_from_time")),
            events_search_to_time: time_text(t.remove("events_search_to_time")),
            events_search_time_preset: str_or(t.remove("events_search_time_preset"), "today"),
            events_search_event_types: event_types,
            events_search_event_types_match_all: bool_or(
                t.remove("events_search_event_types_match_all"),
                false,
            ),
            snapshots_search_from_time: time_text(t.remove("snapshots_search_from_time")),
            snapshots_search_to_time: time_text(t.remove("snapshots_search_to_time")),
            snapshots_search_time_preset: str_or(t.remove("snapshots_search_time_preset"), ""),
            extra: t,
        }
    }
}

/// The three search pages, each with its own persisted filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchPage {
    Recordings,
    Events,
    Snapshots,
}

/// One page's persisted advanced-search filters, as text: the reference
/// parses the times with `datetime.fromisoformat` in local time, which
/// is left to the caller (no timezone library here).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchFilters {
    /// Empty = every camera.
    pub camera_ids: Vec<u32>,
    pub from_time: String,
    pub to_time: String,
    /// `None` for `""` and for a preset the pages do not know.
    pub preset: Option<TimePreset>,
}

// ---------------------------------------------------------------------------
// Camera-keyed per-profile state
// ---------------------------------------------------------------------------

/// One camera's discovered legacy (pre-9.3) event types and how far
/// forward the scan has been brought: `[profiles.<name>.legacy_event_type_history.<id>]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct EventTypeHistory {
    /// `(event type flag, reserved)` pairs.
    pub types: Vec<(i64, i64)>,
    /// Unix time this camera's history has been scanned up to.
    pub checked_until: i64,
}

impl EventTypeHistory {
    fn from_value(v: Value) -> Option<Self> {
        let Value::Table(mut t) = v else {
            return None;
        };
        let types = match t.remove("types") {
            None => Vec::new(),
            Some(Value::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    let Value::Array(pair) = item else {
                        continue;
                    };
                    if pair.len() != 2 {
                        continue;
                    }
                    // A pair that is not two integers fails the whole
                    // entry, as `int()` raising does in the reference.
                    let a = int_of(&pair[0])?;
                    let b = int_of(&pair[1])?;
                    out.push((a, b));
                }
                out
            }
            Some(_) => return None,
        };
        let checked_until = match t.remove("checked_until") {
            None => 0,
            Some(v) => int_of(&v)?,
        };
        Some(Self {
            types,
            checked_until,
        })
    }
}

/// Everything keyed by camera ID, kept per profile because every NAS
/// numbers its cameras from 1. Written inside `[profiles.<name>]`; a
/// table that is empty is not written at all.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct ProfileState {
    /// Camera IDs per grid layout (`"2x2"` → 4 entries); `0` = empty slot.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub layout_cameras: BTreeMap<String, Vec<u32>>,
    /// Direct RTSP URL per camera ID, played by protocol `"direct"`.
    #[serde(
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "ser_camera_map"
    )]
    pub camera_overrides: BTreeMap<u32, String>,
    /// Stream protocol per camera ID, one of [`STREAM_PROTOCOLS`].
    #[serde(
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "ser_camera_map"
    )]
    pub camera_protocols: BTreeMap<u32, String>,
    /// [`StreamProfile`] name per camera ID (`high`, `balanced`, `low`).
    #[serde(
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "ser_camera_map"
    )]
    pub camera_live_view_stream_profiles: BTreeMap<u32, String>,
    /// Live View volume per camera ID, held to 0–100.
    #[serde(
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "ser_camera_map"
    )]
    pub camera_volume: BTreeMap<u32, u8>,
    /// Live View mute per camera ID (a camera with no entry starts muted).
    #[serde(
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "ser_camera_map"
    )]
    pub camera_muted: BTreeMap<u32, bool>,
    /// Fisheye dewarp per camera ID, one of [`DEWARP_MODES`].
    #[serde(
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "ser_camera_map"
    )]
    pub camera_dewarp: BTreeMap<u32, String>,
    /// Lens calibration per camera ID: `"center_x,center_y,radius,fov"`.
    #[serde(
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "ser_camera_map"
    )]
    pub camera_fisheye_lens: BTreeMap<u32, String>,
    /// `"wall"` per camera ID for a lens looking out rather than down.
    #[serde(
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "ser_camera_map"
    )]
    pub camera_fisheye_mount: BTreeMap<u32, String>,
    /// Legacy event-type discovery cache per camera ID. Also read from
    /// the old key `event_type_history`; only this name is written.
    #[serde(
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "ser_camera_map"
    )]
    pub legacy_event_type_history: BTreeMap<u32, EventTypeHistory>,
    /// Cameras of the last Recordings search.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub search_camera_ids: Vec<u32>,
    /// Cameras of the last Events search.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events_search_camera_ids: Vec<u32>,
    /// Cameras of the last Snapshots search.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub snapshots_search_camera_ids: Vec<u32>,
}

/// Keys of [`ProfileState`] as they appear in the file, plus the old
/// `event_type_history` name: what an older file's shared, top-level
/// copy is migrated from.
const PROFILE_STATE_KEYS: &[&str] = &[
    "layout_cameras",
    "camera_overrides",
    "camera_protocols",
    "camera_live_view_stream_profiles",
    "camera_volume",
    "camera_muted",
    "camera_dewarp",
    "camera_fisheye_lens",
    "camera_fisheye_mount",
    "legacy_event_type_history",
    "event_type_history",
    "search_camera_ids",
    "events_search_camera_ids",
    "snapshots_search_camera_ids",
];

impl ProfileState {
    /// True when no camera-keyed setting is held at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Take the camera-keyed keys out of `t`, leaving the rest.
    fn from_table(t: &mut Table) -> Self {
        let layout_cameras = match t.remove("layout_cameras") {
            Some(Value::Table(layouts)) => layouts
                .into_iter()
                .filter_map(|(k, v)| id_list(Some(v)).map(|ids| (k, ids)))
                .collect(),
            _ => BTreeMap::new(),
        };
        // The current key wins per camera over the old one.
        let mut history = camera_map(t.remove("event_type_history"), EventTypeHistory::from_value);
        history.extend(camera_map(
            t.remove("legacy_event_type_history"),
            EventTypeHistory::from_value,
        ));
        Self {
            layout_cameras,
            camera_overrides: camera_map(t.remove("camera_overrides"), text_of),
            camera_protocols: camera_map(t.remove("camera_protocols"), text_of),
            camera_live_view_stream_profiles: camera_map(
                t.remove("camera_live_view_stream_profiles"),
                text_of,
            ),
            camera_volume: camera_map(t.remove("camera_volume"), |v| {
                int_of(&v).map(|n| n.clamp(0, 100) as u8)
            }),
            camera_muted: camera_map(t.remove("camera_muted"), |v| Some(truthy(&v))),
            camera_dewarp: camera_map(t.remove("camera_dewarp"), text_of),
            camera_fisheye_lens: camera_map(t.remove("camera_fisheye_lens"), text_of),
            camera_fisheye_mount: camera_map(t.remove("camera_fisheye_mount"), text_of),
            legacy_event_type_history: history,
            search_camera_ids: id_list(t.remove("search_camera_ids")).unwrap_or_default(),
            events_search_camera_ids: id_list(t.remove("events_search_camera_ids"))
                .unwrap_or_default(),
            snapshots_search_camera_ids: id_list(t.remove("snapshots_search_camera_ids"))
                .unwrap_or_default(),
        }
    }
}

// ---------------------------------------------------------------------------
// [profiles.<name>]
// ---------------------------------------------------------------------------

/// One `[profiles.<name>]` table: how to reach a NAS, plus that NAS's
/// camera-keyed state. Credentials are never here (see [`crate::secrets`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Profile {
    pub host: String,
    pub port: u16,
    pub https: bool,
    /// Verify the TLS certificate; the reference defaults to `false`
    /// because most NAS run a self-signed one.
    pub verify_ssl: bool,
    /// DSM "trust this device" token from an OTP login; skips the OTP
    /// prompt next time. Written only when set.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub device_id: String,
    /// Account name, as a convenience for a login form. Not written by
    /// the reference, which keeps it in the keyring next to the password
    /// (and keeps that copy authoritative, see [`crate::secrets`]);
    /// the reference drops this key again on its next save.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// UI/API language override for this NAS, e.g. `"enu"`. Also an
    /// extension the reference does not write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Camera-keyed settings of this NAS.
    #[serde(flatten)]
    pub state: ProfileState,
    /// Keys this crate does not know, written back unchanged.
    #[serde(flatten)]
    pub extra: Table,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: DEFAULT_PORT,
            https: true,
            verify_ssl: false,
            device_id: String::new(),
            username: None,
            language: None,
            state: ProfileState::default(),
            extra: Table::new(),
        }
    }
}

impl Profile {
    /// A profile for `host` with every other field at its default.
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            ..Self::default()
        }
    }

    /// `https://host:port` (or `http://`), no trailing slash.
    pub fn base_url(&self) -> String {
        let scheme = if self.https { "https" } else { "http" };
        format!("{scheme}://{}:{}", self.host, self.port)
    }

    fn from_table(mut t: Table) -> Self {
        let port = match t.remove("port").as_ref().and_then(int_of) {
            Some(n) if (1..=i64::from(u16::MAX)).contains(&n) => n as u16,
            _ => DEFAULT_PORT,
        };
        let state = ProfileState::from_table(&mut t);
        Self {
            host: str_or(t.remove("host"), ""),
            port,
            https: bool_or(t.remove("https"), true),
            verify_ssl: bool_or(t.remove("verify_ssl"), false),
            device_id: str_or(t.remove("device_id"), ""),
            username: t.remove("username").and_then(text_of),
            language: t.remove("language").and_then(text_of),
            state,
            extra: t,
        }
    }
}

// ---------------------------------------------------------------------------
// The whole file
// ---------------------------------------------------------------------------

/// The whole `config.toml`.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct AppConfig {
    pub general: General,
    pub session: Session,
    /// Settings-page numeric overrides, keyed by setting key; present
    /// only for settings changed from their default (see [`crate::registry`]).
    pub setting_overrides: BTreeMap<String, f64>,
    /// Same, for the Settings page's on/off toggles.
    pub setting_overrides_bool: BTreeMap<String, bool>,
    /// Same, for the Settings page's dropdowns.
    pub setting_overrides_choice: BTreeMap<String, String>,
    /// Connection profiles by name.
    pub profiles: BTreeMap<String, Profile>,
    /// Top-level keys this crate does not know, written back unchanged.
    #[serde(flatten)]
    pub extra: Table,
}

impl AppConfig {
    /// Parse TOML text. Only a syntax error fails; wrong-typed values
    /// fall back to their defaults as described on [`AppConfig`].
    pub fn from_toml_str(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str::<Table>(text).map(Self::from_table)
    }

    /// Serialize to TOML text in the reference's layout.
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string(self)
    }

    /// Build from an already-parsed TOML table.
    pub fn from_table(mut t: Table) -> Self {
        let mut general_table = table_or_empty(t.remove("general"));
        let old_grid_layout = general_table.remove("grid_layout");
        let general = General::from_table(general_table);
        let session_table = table_or_empty(t.remove("session"));

        // An older file kept one shared set of camera-keyed settings at the
        // top level and in [session]; it becomes the default profile's,
        // unless that profile already has its own.
        let mut legacy = Table::new();
        for key in PROFILE_STATE_KEYS {
            if let Some(v) = t.get(*key) {
                legacy.insert((*key).to_string(), v.clone());
            }
            if let Some(v) = session_table.get(*key) {
                legacy.insert((*key).to_string(), v.clone());
            }
        }
        let session = Session::from_table(session_table, old_grid_layout);

        let mut profiles: BTreeMap<String, Profile> = match t.remove("profiles") {
            Some(Value::Table(ps)) => ps
                .into_iter()
                .filter_map(|(name, v)| match v {
                    Value::Table(pt) => Some((name, Profile::from_table(pt))),
                    _ => None,
                })
                .collect(),
            _ => BTreeMap::new(),
        };
        let mut session = session;
        if !legacy.is_empty()
            && let Some(profile) = profiles.get_mut(&general.default_profile)
            && profile.state.is_empty()
        {
            profile.state = ProfileState::from_table(&mut legacy);
            for key in PROFILE_STATE_KEYS {
                t.remove(*key);
                session.extra.remove(*key);
            }
        }

        Self {
            general,
            session,
            setting_overrides: string_map(t.remove("setting_overrides"), |v| {
                let n = match v {
                    Value::Integer(i) => i as f64,
                    Value::Float(f) => f,
                    Value::String(s) => s.trim().parse().ok()?,
                    Value::Boolean(b) => f64::from(u8::from(b)),
                    _ => return None,
                };
                // TOML has nan and inf; no setting takes either.
                n.is_finite().then_some(n)
            }),
            setting_overrides_bool: string_map(t.remove("setting_overrides_bool"), |v| {
                Some(truthy(&v))
            }),
            setting_overrides_choice: string_map(
                t.remove("setting_overrides_choice"),
                |v| match v {
                    Value::String(s) => Some(s),
                    _ => None,
                },
            ),
            profiles,
            extra: t,
        }
    }

    /// The profile named by `general.default_profile`, if it exists.
    pub fn default_profile(&self) -> Option<&Profile> {
        self.profiles.get(&self.general.default_profile)
    }

    /// Mutable [`Self::default_profile`].
    pub fn default_profile_mut(&mut self) -> Option<&mut Profile> {
        self.profiles.get_mut(&self.general.default_profile)
    }

    /// Add or replace a profile; the first one added becomes the default.
    pub fn add_profile(&mut self, name: impl Into<String>, profile: Profile) {
        let name = name.into();
        if self.general.default_profile.is_empty() {
            self.general.default_profile.clone_from(&name);
        }
        self.profiles.insert(name, profile);
    }

    /// Remove a profile. A removed default profile leaves
    /// `default_profile` pointing at the first remaining one, or `""`.
    pub fn remove_profile(&mut self, name: &str) -> Option<Profile> {
        let removed = self.profiles.remove(name);
        if self.general.default_profile == name {
            self.general.default_profile = self.profiles.keys().next().cloned().unwrap_or_default();
        }
        removed
    }

    /// One page's persisted search filters for `profile` (the camera
    /// list lives with the profile, the times and preset in `[session]`).
    pub fn search_filters(&self, page: SearchPage, profile: &str) -> SearchFilters {
        let state = self.profiles.get(profile).map(|p| &p.state);
        let s = &self.session;
        let (camera_ids, from, to, preset) = match page {
            SearchPage::Recordings => (
                state.map(|st| &st.search_camera_ids),
                &s.search_from_time,
                &s.search_to_time,
                &s.search_time_preset,
            ),
            SearchPage::Events => (
                state.map(|st| &st.events_search_camera_ids),
                &s.events_search_from_time,
                &s.events_search_to_time,
                &s.events_search_time_preset,
            ),
            SearchPage::Snapshots => (
                state.map(|st| &st.snapshots_search_camera_ids),
                &s.snapshots_search_from_time,
                &s.snapshots_search_to_time,
                &s.snapshots_search_time_preset,
            ),
        };
        SearchFilters {
            camera_ids: camera_ids.cloned().unwrap_or_default(),
            from_time: from.clone(),
            to_time: to.clone(),
            preset: TimePreset::parse(preset),
        }
    }

    /// Persist one page's search filters; the inverse of [`Self::search_filters`].
    /// Camera IDs are stored only when `profile` exists.
    pub fn set_search_filters(&mut self, page: SearchPage, profile: &str, filters: &SearchFilters) {
        let preset = filters.preset.map_or("", TimePreset::as_str).to_string();
        let s = &mut self.session;
        match page {
            SearchPage::Recordings => {
                s.search_from_time.clone_from(&filters.from_time);
                s.search_to_time.clone_from(&filters.to_time);
                s.search_time_preset = preset;
            }
            SearchPage::Events => {
                s.events_search_from_time.clone_from(&filters.from_time);
                s.events_search_to_time.clone_from(&filters.to_time);
                s.events_search_time_preset = preset;
            }
            SearchPage::Snapshots => {
                s.snapshots_search_from_time.clone_from(&filters.from_time);
                s.snapshots_search_to_time.clone_from(&filters.to_time);
                s.snapshots_search_time_preset = preset;
            }
        }
        if let Some(p) = self.profiles.get_mut(profile) {
            let ids = match page {
                SearchPage::Recordings => &mut p.state.search_camera_ids,
                SearchPage::Events => &mut p.state.events_search_camera_ids,
                SearchPage::Snapshots => &mut p.state.snapshots_search_camera_ids,
            };
            ids.clone_from(&filters.camera_ids);
        }
    }
}

impl<'de> Deserialize<'de> for AppConfig {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Table::deserialize(d).map(Self::from_table)
    }
}

// ---------------------------------------------------------------------------
// Lenient value helpers
// ---------------------------------------------------------------------------

fn table_or_empty(v: Option<Value>) -> Table {
    match v {
        Some(Value::Table(t)) => t,
        _ => Table::new(),
    }
}

fn str_or(v: Option<Value>, default: &str) -> String {
    match v {
        Some(Value::String(s)) => s,
        _ => default.to_string(),
    }
}

fn bool_or(v: Option<Value>, default: bool) -> bool {
    match v {
        Some(Value::Boolean(b)) => b,
        _ => default,
    }
}

/// `int()` as the reference applies it: an integer, a float truncated,
/// or a string holding an integer.
fn int_of(v: &Value) -> Option<i64> {
    match v {
        Value::Integer(i) => Some(*i),
        Value::Float(f) if f.is_finite() => Some(f.trunc() as i64),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `str()` as the reference applies it to a camera table value, for the
/// scalar types that make sense as one.
fn text_of(v: Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s),
        Value::Integer(i) => Some(i.to_string()),
        Value::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

/// Python truthiness, for the reference's `bool(value)`.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Boolean(b) => *b,
        Value::Integer(i) => *i != 0,
        Value::Float(f) => *f != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Table(t) => !t.is_empty(),
        Value::Datetime(_) => true,
    }
}

/// A search time as text; an unquoted TOML datetime becomes its ISO text.
fn time_text(v: Option<Value>) -> String {
    match v {
        Some(Value::String(s)) => s,
        Some(Value::Datetime(dt)) => dt.to_string(),
        _ => String::new(),
    }
}

/// An array of camera IDs; `None` when it is not an array (a bad element
/// is skipped).
fn id_list(v: Option<Value>) -> Option<Vec<u32>> {
    match v {
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .filter_map(|v| int_of(v).and_then(|n| u32::try_from(n).ok()))
                .collect(),
        ),
        _ => None,
    }
}

/// A table keyed by camera ID, dropping entries whose key is not an
/// integer or whose value `convert` rejects.
fn camera_map<T>(v: Option<Value>, convert: impl Fn(Value) -> Option<T>) -> BTreeMap<u32, T> {
    let mut out = BTreeMap::new();
    if let Some(Value::Table(t)) = v {
        for (k, v) in t {
            let Ok(id) = k.trim().parse::<u32>() else {
                continue;
            };
            if let Some(value) = convert(v) {
                out.insert(id, value);
            }
        }
    }
    out
}

fn string_map<T>(v: Option<Value>, convert: impl Fn(Value) -> Option<T>) -> BTreeMap<String, T> {
    let mut out = BTreeMap::new();
    if let Some(Value::Table(t)) = v {
        for (k, v) in t {
            if let Some(value) = convert(v) {
                out.insert(k, value);
            }
        }
    }
    out
}

/// Camera IDs are integers in memory but TOML keys are text.
fn ser_camera_map<S: Serializer, T: Serialize>(
    m: &BTreeMap<u32, T>,
    s: S,
) -> Result<S::Ok, S::Error> {
    let mut map = s.serialize_map(Some(m.len()))?;
    for (k, v) in m {
        map.serialize_entry(&k.to_string(), v)?;
    }
    map.end()
}
