//! `/ss_webstream_task/` connect URLs for live and history streams.
//!
//! Mirrors the reference's `services/live.py` (`_build_ws_live_url`,
//! `get_history_view_path`): `wss` when the DSM base URL is `https`,
//! `ws` otherwise, and everything after the scheme reused verbatim as
//! `host[:port]`.
//!
//! The session is **not** carried in the URL. The reference authenticates
//! the WebSocket purely with a `Cookie: id=<sid>` handshake header
//! (`ws_bridge.py` `_pump`, `headers = {"Cookie": f"id={self._sid}"}` passed
//! as `additional_headers`), which is also what the official desktop
//! client's `video_plugin.dll` does (`docs/architecture.md` §3.2,
//! "Session is passed as `Cookie: id=…`"). No `_sid` query parameter.

use crate::error::UrlError;

/// Live-view stream quality, as the `profile=` number DSM expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum StreamProfile {
    /// `profile=0`.
    High = 0,
    /// `profile=1`.
    #[default]
    Balanced = 1,
    /// `profile=2`.
    Low = 2,
}

impl StreamProfile {
    /// The number sent on the wire.
    pub fn number(self) -> u8 {
        self as u8
    }

    /// The profile for a DSM `defLiveProfile` value, `None` if out of range.
    pub fn from_number(n: u8) -> Option<Self> {
        match n {
            0 => Some(Self::High),
            1 => Some(Self::Balanced),
            2 => Some(Self::Low),
            _ => None,
        }
    }
}

/// `ws[s]://host[:port]` for a DSM base URL such as `https://nas:5001`.
///
/// Trailing slashes are dropped; a path prefix (reverse proxy) is kept.
fn ws_origin(base_url: &str) -> Result<String, UrlError> {
    let base = base_url.trim();
    let (scheme, rest) = if let Some(rest) = base.strip_prefix("https://") {
        ("wss", rest)
    } else if let Some(rest) = base.strip_prefix("wss://") {
        ("wss", rest)
    } else if let Some(rest) = base.strip_prefix("http://") {
        ("ws", rest)
    } else if let Some(rest) = base.strip_prefix("ws://") {
        ("ws", rest)
    } else {
        return Err(UrlError::UnsupportedScheme);
    };
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() || rest.starts_with('/') {
        return Err(UrlError::MissingHost);
    }
    Ok(format!("{scheme}://{rest}"))
}

/// The live MixStream URL for one camera:
/// `…/ss_webstream_task/?method=MixStream&stmSrc=0&blAudio=true&dsId=0&id=<camera>&devType=1&profile=<n>`.
pub fn live_url(
    base_url: &str,
    camera_id: u32,
    profile: StreamProfile,
) -> Result<String, UrlError> {
    let origin = ws_origin(base_url)?;
    Ok(format!(
        "{origin}/ss_webstream_task/?method=MixStream&stmSrc=0&blAudio=true\
         &dsId=0&id={camera_id}&devType=1&profile={}",
        profile.number()
    ))
}

/// The history (recorded playback) MixStream URL:
/// `…/ss_webstream_task/?method=MixStream&stmSrc=2&dsId=0&id=0`.
///
/// Nothing in it selects a recording; that is done by the in-band
/// `action=play` message (see [`crate::command::history_play`]). The
/// reference deliberately leaves out the `blMux=true&browser=2&
/// relay_rec_auth=false` the browser also puts in this URL: `blMux=true`
/// at connect level switches DSM to a muxed MP4 container instead of
/// the raw frames this crate parses.
pub fn history_url(base_url: &str) -> Result<String, UrlError> {
    let origin = ws_origin(base_url)?;
    Ok(format!(
        "{origin}/ss_webstream_task/?method=MixStream&stmSrc=2&dsId=0&id=0"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_https_becomes_wss() {
        assert_eq!(
            live_url("https://nas:5001", 7, StreamProfile::Balanced).unwrap(),
            "wss://nas:5001/ss_webstream_task/?method=MixStream&stmSrc=0&blAudio=true\
             &dsId=0&id=7&devType=1&profile=1"
        );
    }

    #[test]
    fn live_http_becomes_ws_and_profiles_map() {
        assert_eq!(
            live_url("http://10.0.0.2:5000/", 12, StreamProfile::High).unwrap(),
            "ws://10.0.0.2:5000/ss_webstream_task/?method=MixStream&stmSrc=0&blAudio=true\
             &dsId=0&id=12&devType=1&profile=0"
        );
        assert!(
            live_url("http://h", 1, StreamProfile::Low)
                .unwrap()
                .ends_with("profile=2")
        );
    }

    #[test]
    fn history_url_is_fixed() {
        assert_eq!(
            history_url("https://nas.example.com:5001").unwrap(),
            "wss://nas.example.com:5001/ss_webstream_task/?method=MixStream&stmSrc=2&dsId=0&id=0"
        );
        assert_eq!(
            history_url("http://nas").unwrap(),
            "ws://nas/ss_webstream_task/?method=MixStream&stmSrc=2&dsId=0&id=0"
        );
    }

    #[test]
    fn path_prefix_is_kept() {
        assert_eq!(
            history_url("https://proxy/dsm/").unwrap(),
            "wss://proxy/dsm/ss_webstream_task/?method=MixStream&stmSrc=2&dsId=0&id=0"
        );
    }

    #[test]
    fn bad_bases_are_rejected() {
        assert_eq!(history_url("nas:5001"), Err(UrlError::UnsupportedScheme));
        assert_eq!(history_url("https://"), Err(UrlError::MissingHost));
        assert_eq!(history_url("https:///x"), Err(UrlError::MissingHost));
    }

    #[test]
    fn profile_numbers_round_trip() {
        for n in 0..3 {
            assert_eq!(StreamProfile::from_number(n).unwrap().number(), n);
        }
        assert_eq!(StreamProfile::from_number(3), None);
    }
}
