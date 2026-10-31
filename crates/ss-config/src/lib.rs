//! Configuration, settings and secrets for a Linux Surveillance Station
//! client, file-compatible with the reference Python/GTK client
//! (renaudallard/surveillance-station-client): the same
//! `~/.config/surveillance-station/config.toml`, the same keys and
//! defaults, and the same keyring entries, so a user can switch between
//! the two.
//!
//! # Layout of the file
//!
//! | Section | Holds |
//! |---|---|
//! | `[general]` ([`General`]) | default profile, theme, panel visibility, poll intervals, snapshot folder |
//! | `[session]` ([`Session`]) | grid layout, last page, search times and presets, event-type filter |
//! | `[setting_overrides]`, `[setting_overrides_bool]`, `[setting_overrides_choice]` | Settings-page overrides, see [`registry`] |
//! | `[profiles.<name>]` ([`Profile`]) | host, port, https, verify_ssl, device_id, and that NAS's camera-keyed tables ([`ProfileState`]): named layouts (`[[layouts]]`), camera panel order, per-camera protocol, RTSP override, stream profile, volume, mute, fisheye dewarp/lens/mount, legacy event-type cache, search camera lists |
//!
//! Any key this crate does not know is kept in the `extra` table of the
//! section it was found in and written back unchanged, so a newer
//! reference's data survives a round trip through this client.
//!
//! # Example
//!
//! ```no_run
//! use ss_config::{AppConfig, Paths, Profile, load_or_salvage, save_to};
//!
//! let paths = Paths::from_env()?;
//! let file = paths.config_file();
//! let mut config = load_or_salvage(&file).config;
//! config.add_profile("home-nas", Profile::new("192.168.1.100"));
//! config.set_setting("osd_enabled", ss_config::SettingValue::Bool(true))?;
//! save_to(&config, &file)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Secrets
//!
//! Passwords never go in the file. [`KeyringStore`] keeps them in the
//! Secret Service under service `surveillance-station`, users
//! `<profile>:username` and `<profile>:password`, exactly as the
//! reference does; [`SecretError::Unavailable`] is the typed signal that
//! there is no keyring to talk to (headless, tests), and
//! [`MemoryStore`] stands in for it.

mod io;
mod model;
mod paths;
pub mod registry;
pub mod secrets;

pub use io::{ConfigError, Loaded, load_from, load_or_salvage, save_to};
pub use model::{
    AppConfig, DEFAULT_PORT, DEWARP_MODES, EventTypeHistory, FISHEYE_MOUNT_WALL, GRID_LAYOUTS,
    General, LAYOUT_SHAPES, LayoutDef, MIN_POLL_INTERVAL, Profile, ProfileState, STREAM_PROTOCOLS,
    SearchFilters, SearchPage, Session, StreamProfile, TIMELINE_EVENT_KINDS, Theme, TimePreset,
    default_snapshot_dir, shape_slots,
};
pub use paths::{APP_DIR_NAME, CONFIG_FILE_NAME, Paths};
pub use registry::{SETTINGS, SettingDef, SettingError, SettingKind, SettingValue};
pub use secrets::{
    CredentialField, Credentials, KEYRING_SERVICE, KeyringStore, MemoryStore, SecretError,
    SecretStore, keyring_user,
};

/// Load the configuration from its standard location, salvaging an
/// unreadable file as [`load_or_salvage`] does.
pub fn load() -> Result<Loaded, ConfigError> {
    Ok(load_or_salvage(&Paths::from_env()?.config_file()))
}

/// Save the configuration to its standard location, atomically.
pub fn save(config: &AppConfig) -> Result<(), ConfigError> {
    save_to(config, &Paths::from_env()?.config_file())
}
