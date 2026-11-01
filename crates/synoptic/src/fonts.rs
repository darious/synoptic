//! Bundled IBM Plex Sans/Mono (OFL-1.1, `data/fonts/`). The UI is designed
//! around these; without them fontconfig picks whatever the machine has
//! (Verdana on a bare Windows box, 25 % wider), every label grows and the
//! window's minimum width no longer fits a small screen. The files are
//! written to the cache dir once and registered as application fonts with
//! fontconfig before GTK creates its first font map, so the CSS
//! `"IBM Plex Sans"` resolves everywhere Pango uses fontconfig (Linux,
//! MSYS2 Windows). macOS Pango uses CoreText; there the fallback chain in
//! `style.css` applies.

#[cfg(not(target_os = "macos"))]
const FONTS: [(&str, &[u8]); 7] = [
    (
        "IBMPlexSans-Regular.ttf",
        include_bytes!("../data/fonts/IBMPlexSans-Regular.ttf"),
    ),
    (
        "IBMPlexSans-Medium.ttf",
        include_bytes!("../data/fonts/IBMPlexSans-Medium.ttf"),
    ),
    (
        "IBMPlexSans-SemiBold.ttf",
        include_bytes!("../data/fonts/IBMPlexSans-SemiBold.ttf"),
    ),
    (
        "IBMPlexSans-Bold.ttf",
        include_bytes!("../data/fonts/IBMPlexSans-Bold.ttf"),
    ),
    (
        "IBMPlexMono-Regular.ttf",
        include_bytes!("../data/fonts/IBMPlexMono-Regular.ttf"),
    ),
    (
        "IBMPlexMono-Medium.ttf",
        include_bytes!("../data/fonts/IBMPlexMono-Medium.ttf"),
    ),
    (
        "IBMPlexMono-SemiBold.ttf",
        include_bytes!("../data/fonts/IBMPlexMono-SemiBold.ttf"),
    ),
];

/// Unpack the bundled fonts and register their directory with fontconfig.
/// Call before `gtk::Application` is built.
#[cfg(not(target_os = "macos"))]
pub fn install() {
    use std::ffi::CString;

    let Ok(paths) = ss_config::Paths::from_env() else {
        return;
    };
    let dir = paths.cache_dir.join("fonts");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::info!("bundled fonts: cannot create {}: {e}", dir.display());
        return;
    }
    for (name, bytes) in FONTS {
        let path = dir.join(name);
        let same = std::fs::metadata(&path).is_ok_and(|m| m.len() == bytes.len() as u64);
        if !same && let Err(e) = std::fs::write(&path, bytes) {
            tracing::info!("bundled fonts: cannot write {}: {e}", path.display());
            return;
        }
    }
    let Ok(c_dir) = CString::new(dir.to_string_lossy().as_bytes()) else {
        return;
    };
    // SAFETY: a NUL-terminated path; NULL config = fontconfig's current one.
    let ok = unsafe {
        fontconfig_sys::FcConfigAppFontAddDir(
            std::ptr::null_mut(),
            c_dir.as_ptr() as *const fontconfig_sys::FcChar8,
        )
    };
    if ok == 0 {
        tracing::info!("bundled fonts: fontconfig refused {}", dir.display());
    } else {
        tracing::debug!("bundled fonts registered from {}", dir.display());
    }
}

#[cfg(target_os = "macos")]
pub fn install() {}
