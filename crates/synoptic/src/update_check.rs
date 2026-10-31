//! Release check: once per start, five seconds after the first login, ask
//! GitHub for the latest release of this repository and, when its tag is
//! a higher semver than `CARGO_PKG_VERSION` and not the version the user
//! dismissed (`general.dismissed_update_version`), raise
//! `AppState::update_notice_unread` so the rail dot and the About notice
//! appear. Network errors and a 404 (no releases yet) are debug-logged and
//! nothing is shown. The About page reads the found release via [`latest`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;

use crate::services::Services;
use crate::state::{Change, Store};

/// `repository` from Cargo.toml (`https://github.com/<owner>/<repo>`).
const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// Delay after login before the request goes out.
const DELAY: Duration = Duration::from_secs(5);
const TIMEOUT: Duration = Duration::from_secs(10);

/// What GitHub told us about the newest release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// `1.2.3` (the tag with a leading `v` stripped).
    pub version: String,
    /// Release page.
    pub url: String,
}

thread_local! {
    static LATEST: RefCell<Option<Release>> = const { RefCell::new(None) };
}

/// The newer release found this session, if any.
pub fn latest() -> Option<Release> {
    LATEST.with(|l| l.borrow().clone())
}

/// `https://api.github.com/repos/<owner>/<repo>/releases/latest` for the
/// manifest's repository URL; `None` when it is not a GitHub URL.
pub fn api_url(repo_url: &str) -> Option<String> {
    let path = repo_url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .strip_prefix("https://github.com/")?;
    let (owner, repo) = path.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some(format!(
        "https://api.github.com/repos/{owner}/{repo}/releases/latest"
    ))
}

/// `v1.2.3-rc1` → `(1, 2, 3, Some("rc1"))`; `None` if the core is not `x.y.z`.
pub fn parse_version(tag: &str) -> Option<(u64, u64, u64, Option<&str>)> {
    let tag = tag.trim().trim_start_matches(['v', 'V']);
    let (core, pre) = match tag.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (tag.split_once('+').map(|(c, _)| c).unwrap_or(tag), None),
    };
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let (a, b, c) = (it.next()??, it.next()??, it.next()??);
    if it.next().is_some() {
        return None;
    }
    Some((a, b, c, pre))
}

/// `true` when `remote` is a newer release than `current`. A pre-release
/// tag is only newer when its core already beats the current version; an
/// equal core never is (`1.2.0-rc1` is not newer than `1.2.0`).
pub fn is_newer(remote: &str, current: &str) -> bool {
    match (parse_version(remote), parse_version(current)) {
        (Some((ra, rb, rc, _)), Some((ca, cb, cc, _))) => (ra, rb, rc) > (ca, cb, cc),
        _ => false,
    }
}

/// Pull `tag_name` / `html_url` out of GitHub's release JSON.
pub fn parse_release(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = v.get("tag_name")?.as_str()?;
    let version = tag.trim().trim_start_matches(['v', 'V']).to_string();
    let url = v
        .get("html_url")
        .and_then(|u| u.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{REPO_URL}/releases"));
    Some(Release { version, url })
}

async fn fetch(url: String) -> Result<Option<Release>, String> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(format!("synoptic/{CURRENT}"))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body = resp.text().await.map_err(|e| e.to_string())?;
    Ok(parse_release(&body))
}

/// Subscribe to the store: schedules the check after the first login.
pub fn install(store: &Rc<Store>, services: &Rc<Services>) {
    let Some(url) = api_url(REPO_URL) else {
        tracing::debug!("update check: no GitHub repository in the manifest");
        return;
    };
    let scheduled = Rc::new(Cell::new(false));
    let store_w = Rc::downgrade(store);
    let services = services.clone();
    store.subscribe(move |change| {
        if !matches!(change, Change::Connection) || scheduled.get() {
            return;
        }
        let Some(store) = store_w.upgrade() else {
            return;
        };
        if !store.get().logged_in() {
            return;
        }
        scheduled.set(true);
        let store_w = Rc::downgrade(&store);
        let services = services.clone();
        let url = url.clone();
        glib::timeout_add_local_once(DELAY, move || {
            let Some(store) = store_w.upgrade() else {
                return;
            };
            let fut = services.run(fetch(url));
            glib::spawn_future_local(async move {
                match fut.await {
                    Ok(Some(rel)) => apply(&store, rel),
                    Ok(None) => tracing::debug!("update check: no releases published"),
                    Err(e) => tracing::debug!("update check: {e}"),
                }
            });
        });
    });
}

/// Treat `rel` as the latest published release (what the check does on a
/// successful fetch; also the headless driver's `release` command).
pub fn announce(store: &Rc<Store>, rel: Release) {
    apply(store, rel);
}

fn apply(store: &Rc<Store>, rel: Release) {
    if !is_newer(&rel.version, CURRENT) {
        tracing::debug!(
            "update check: {} is current (latest {})",
            CURRENT,
            rel.version
        );
        return;
    }
    let dismissed = store.config().general.dismissed_update_version.clone();
    if dismissed == rel.version {
        tracing::debug!("update check: {} available but dismissed", rel.version);
        return;
    }
    tracing::info!(
        "update check: {} available (running {})",
        rel.version,
        CURRENT
    );
    LATEST.with(|l| *l.borrow_mut() = Some(rel));
    store.update(Change::UpdateNotice, |s| s.update_notice_unread = true);
}

/// About's Dismiss: remember the version so it is not announced again.
pub fn dismiss(store: &Rc<Store>) {
    if let Some(rel) = latest() {
        store.config().general.dismissed_update_version = rel.version;
        store.save_soon();
    }
    store.update(Change::UpdateNotice, |s| s.update_notice_unread = false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_url_from_repository() {
        assert_eq!(
            api_url("https://github.com/darious/synoptic").as_deref(),
            Some("https://api.github.com/repos/darious/synoptic/releases/latest")
        );
        assert_eq!(
            api_url("https://github.com/darious/synoptic.git/"),
            api_url("https://github.com/darious/synoptic")
        );
        assert_eq!(api_url("https://gitlab.com/x/y"), None);
        assert_eq!(api_url(""), None);
        assert!(api_url(REPO_URL).is_some());
    }

    #[test]
    fn semver_compare() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0-rc1", "0.1.0"));
        assert!(is_newer("0.2.0-rc1", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
        assert!(!is_newer("1.2", "0.1.0"));
        assert_eq!(parse_version("v1.2.3+build"), Some((1, 2, 3, None)));
    }

    #[test]
    fn release_json() {
        let r = parse_release(
            r#"{"tag_name":"v0.3.1","html_url":"https://github.com/d/s/releases/tag/v0.3.1"}"#,
        )
        .unwrap();
        assert_eq!(r.version, "0.3.1");
        assert!(r.url.ends_with("/v0.3.1"));
        assert!(parse_release(r#"{"message":"Not Found"}"#).is_none());
        assert!(parse_release("not json").is_none());
    }
}
