//! Reading and writing `config.toml`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::model::AppConfig;

/// What can go wrong loading, saving or locating the config file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no home directory to resolve XDG paths from")]
    NoHome,
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not valid TOML: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("cannot serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
}

/// Load `path`. A missing file is the default configuration; a file that
/// is not TOML is [`ConfigError::Parse`].
pub fn load_from(path: &Path) -> Result<AppConfig, ConfigError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(AppConfig::default()),
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    AppConfig::from_toml_str(&text).map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// What [`load_or_salvage`] did.
#[derive(Debug)]
pub struct Loaded {
    pub config: AppConfig,
    /// Where an unreadable file was moved, when it was.
    pub salvaged_to: Option<PathBuf>,
}

/// Load `path` the way the reference does: a file that cannot be read or
/// parsed is moved aside to `config.toml.bad` and the defaults are used,
/// so the next save does not overwrite whatever was still recoverable.
pub fn load_or_salvage(path: &Path) -> Loaded {
    match load_from(path) {
        Ok(config) => Loaded {
            config,
            salvaged_to: None,
        },
        Err(e) => {
            let bad = bad_path(path);
            let salvaged_to = fs::rename(path, &bad).ok().map(|()| bad.clone());
            tracing::error!(
                "Unreadable config, starting with defaults; kept a copy at {}: {e}",
                bad.display()
            );
            Loaded {
                config: AppConfig::default(),
                salvaged_to,
            }
        }
    }
}

fn bad_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bad");
    path.with_file_name(name)
}

/// Write `config` to `path` atomically: a sibling temp file is written,
/// flushed to disk and renamed over the real one, so an interrupted save
/// leaves the previous file intact. The file is created mode 0600 and
/// missing parent directories are made.
pub fn save_to(config: &AppConfig, path: &Path) -> Result<(), ConfigError> {
    let text = config.to_toml_string()?;
    let write_err = |source: io::Error| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(write_err)?;
    }
    // Per-process temp name: two instances sharing a $HOME must not
    // interleave writes into one temp file.
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".{}.new", std::process::id()));
    let tmp = path.with_file_name(tmp_name);

    let result = write_and_rename(&tmp, path, text.as_bytes());
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(write_err)
}

fn write_and_rename(tmp: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(tmp, path)
}
