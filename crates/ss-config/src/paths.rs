//! XDG base-directory resolution, same directories as the reference.
//!
//! The reference (`surveillance/config.py`) puts everything under an
//! app directory named [`APP_DIR_NAME`] inside each XDG base directory:
//! `$XDG_CONFIG_HOME/surveillance-station/config.toml` and so on, with
//! the usual `~/.config`, `~/.local/share`, `~/.local/state` and
//! `~/.cache` fallbacks.

use std::path::{Path, PathBuf};

use directories::BaseDirs;

use crate::ConfigError;

/// Directory name under each XDG base directory.
pub const APP_DIR_NAME: &str = "surveillance-station";
/// File name of the configuration file inside the config directory.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// The application's directories, resolved once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `$XDG_CONFIG_HOME/surveillance-station`: the config file.
    pub config_dir: PathBuf,
    /// `$XDG_DATA_HOME/surveillance-station`: user content such as snapshots.
    pub data_dir: PathBuf,
    /// `$XDG_STATE_HOME/surveillance-station`: logs.
    pub state_dir: PathBuf,
    /// `$XDG_CACHE_HOME/surveillance-station`: files only needed while running.
    pub cache_dir: PathBuf,
}

impl Paths {
    /// Resolve from the environment (`XDG_*` variables, then `$HOME`).
    ///
    /// Reads the environment on every call, so a changed `XDG_CONFIG_HOME`
    /// is honoured by the next call. Fails only when no home directory can
    /// be determined.
    pub fn from_env() -> Result<Self, ConfigError> {
        let base = BaseDirs::new().ok_or(ConfigError::NoHome)?;
        let state_dir = base
            .state_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| base.home_dir().join(".local").join("state"));
        Ok(Self::from_base_dirs(
            base.config_dir(),
            base.data_dir(),
            &state_dir,
            base.cache_dir(),
        ))
    }

    /// Resolve from explicit base directories (what `XDG_CONFIG_HOME` etc.
    /// would hold), for tests and embedding.
    pub fn from_base_dirs(config: &Path, data: &Path, state: &Path, cache: &Path) -> Self {
        Self {
            config_dir: config.join(APP_DIR_NAME),
            data_dir: data.join(APP_DIR_NAME),
            state_dir: state.join(APP_DIR_NAME),
            cache_dir: cache.join(APP_DIR_NAME),
        }
    }

    /// `<config_dir>/config.toml`.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE_NAME)
    }

    /// `<data_dir>/snapshots`: the reference's default `snapshot_dir`.
    pub fn default_snapshot_dir(&self) -> PathBuf {
        self.data_dir.join("snapshots")
    }
}
