//! Launcher integration without an install step. On Wayland a window gets
//! its icon only through a desktop entry matching the application id, and
//! `cargo run` installs none; so at startup the app writes (or refreshes)
//! `io.github.darious.Synoptic.desktop` and the icons under
//! `$XDG_DATA_HOME` (`~/.local/share`), pointing `Exec` at the running
//! binary. Files are rewritten only when their content differs, and
//! nothing else in those directories is touched. `tools/install.sh` does
//! the same for a release build.

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
use std::path::{Path, PathBuf};

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
use crate::APP_ID;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const ICON: &str = include_str!("../data/icons/io.github.darious.Synoptic.svg");
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const ICON_SYMBOLIC: &str = include_str!("../data/icons/io.github.darious.Synoptic-symbolic.svg");
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const DESKTOP: &str = include_str!("../data/io.github.darious.Synoptic.desktop");

/// `$XDG_DATA_HOME`, else `~/.local/share`.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn data_home() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("XDG_DATA_HOME").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share"))
}

/// The desktop entry with `Exec` pointing at `exe`.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn desktop_entry(exe: &Path) -> String {
    let exe = exe.to_string_lossy();
    let quoted = if exe.contains(' ') {
        format!("\"{exe}\"")
    } else {
        exe.to_string()
    };
    DESKTOP
        .lines()
        .map(|l| {
            if l.starts_with("Exec=") {
                format!("Exec={quoted}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn write_if_changed(path: &Path, content: &str) -> std::io::Result<bool> {
    if std::fs::read_to_string(path).is_ok_and(|cur| cur == content) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, content)?;
    Ok(true)
}

/// Make sure the launcher entry and icons exist for this binary.
/// Freedesktop platforms only: on macOS the bundle carries the icon, on
/// Windows the executable's resources do.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub fn ensure() {}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn ensure() {
    let Some(data) = data_home() else { return };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let files = [
        (
            data.join(format!("applications/{APP_ID}.desktop")),
            desktop_entry(&exe),
        ),
        (
            data.join(format!("icons/hicolor/scalable/apps/{APP_ID}.svg")),
            ICON.to_string(),
        ),
        (
            data.join(format!("icons/hicolor/symbolic/apps/{APP_ID}-symbolic.svg")),
            ICON_SYMBOLIC.to_string(),
        ),
    ];
    let mut changed = false;
    for (path, content) in &files {
        match write_if_changed(path, content) {
            Ok(c) => changed |= c,
            Err(e) => {
                tracing::info!("launcher entry: cannot write {}: {e}", path.display());
                return;
            }
        }
    }
    if changed {
        tracing::info!("launcher entry and icons written under {}", data.display());
        // icon caches: best effort, both tools are optional
        let icons = data.join("icons/hicolor");
        let _ = std::process::Command::new("gtk-update-icon-cache")
            .args(["-q", "-t"])
            .arg(&icons)
            .status();
        let _ = std::process::Command::new("update-desktop-database")
            .arg(data.join("applications"))
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn exec_points_at_the_binary_and_keeps_the_rest() {
        let e = desktop_entry(Path::new("/opt/syn optic/synoptic"));
        assert!(e.contains("Exec=\"/opt/syn optic/synoptic\"\n"));
        assert!(e.contains(&format!("Icon={APP_ID}\n")));
        assert!(e.contains("Name=Synoptic\n"));
        let e = desktop_entry(Path::new("/usr/bin/synoptic"));
        assert!(e.contains("Exec=/usr/bin/synoptic\n"));
    }

    #[test]
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn write_if_changed_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("synoptic-desktop-{}", std::process::id()));
        let p = dir.join("a/b.txt");
        assert!(write_if_changed(&p, "x").unwrap());
        assert!(!write_if_changed(&p, "x").unwrap());
        assert!(write_if_changed(&p, "y").unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
