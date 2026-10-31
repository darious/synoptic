//! Browse pages: Recordings, Snapshots, Events, Time Lapse.
//!
//! Stage rows `auto 1fr 48px`: the shared filter bar ([`filter_bar`]), the
//! page's content and the pagination footer ([`pagination`]). One
//! [`Browse`] controller per page holds the [`FilterState`], persists it
//! through `ss_config` and drives reloads; the content modules only know
//! how to fetch and render one page of results.
//!
//! Pure helpers (preset → time range, summary text, formatting) live here so
//! they can be unit-tested without a display.

pub mod events;
pub mod filter_bar;
pub mod pagination;
pub mod recordings;
pub mod snapshots;
pub mod timelapse;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::task::{Context, Poll};

use chrono::{Duration, Local, NaiveDateTime, TimeZone};
use gtk::glib;
use gtk::prelude::*;
use ss_api::{ApiRequest, Client};

use crate::services::Services;
use crate::state::{Change, Page, Store};

// ---------------------------------------------------------------------------
// Filter model
// ---------------------------------------------------------------------------

/// Quick-filter presets of the filter bar (row 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Today,
    Yesterday,
    Last24h,
    Last7d,
}

impl Preset {
    pub const ALL: [Preset; 4] = [
        Preset::Today,
        Preset::Yesterday,
        Preset::Last24h,
        Preset::Last7d,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Today => "Today",
            Preset::Yesterday => "Yesterday",
            Preset::Last24h => "Last 24 hrs",
            Preset::Last7d => "Last 7 days",
        }
    }

    /// `[start, end)` in local wall-clock time for a given `now`.
    pub fn range(self, now: NaiveDateTime) -> (NaiveDateTime, NaiveDateTime) {
        let day_start = now.date().and_hms_opt(0, 0, 0).unwrap_or(now);
        match self {
            Preset::Today => (day_start, day_start + Duration::days(1)),
            Preset::Yesterday => (day_start - Duration::days(1), day_start),
            Preset::Last24h => (now - Duration::hours(24), now),
            Preset::Last7d => (now - Duration::days(7), now),
        }
    }

    fn to_config(self) -> ss_config::TimePreset {
        match self {
            Preset::Today => ss_config::TimePreset::Today,
            Preset::Yesterday => ss_config::TimePreset::Yesterday,
            Preset::Last24h => ss_config::TimePreset::Last24h,
            Preset::Last7d => ss_config::TimePreset::Last7d,
        }
    }

    fn from_config(p: ss_config::TimePreset) -> Option<Preset> {
        match p {
            ss_config::TimePreset::Today => Some(Preset::Today),
            ss_config::TimePreset::Yesterday => Some(Preset::Yesterday),
            ss_config::TimePreset::Last24h => Some(Preset::Last24h),
            ss_config::TimePreset::Last7d => Some(Preset::Last7d),
            // The prototype has no "Last 30 days" pill; fall back to the
            // advanced range (or Today).
            ss_config::TimePreset::Last30d => None,
        }
    }
}

/// Event type pills of the Events page (one at a time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventType {
    Motion,
    People,
    Vehicle,
    Audio,
    Tampering,
}

impl EventType {
    pub const ALL: [EventType; 5] = [
        EventType::Motion,
        EventType::People,
        EventType::Vehicle,
        EventType::Audio,
        EventType::Tampering,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EventType::Motion => "Motion",
            EventType::People => "People",
            EventType::Vehicle => "Vehicle",
            EventType::Audio => "Audio",
            EventType::Tampering => "Tampering",
        }
    }

    /// Suffix of the `type-*` / `bg-*` CSS classes.
    pub fn css(self) -> &'static str {
        match self {
            EventType::Motion => "motion",
            EventType::People => "people",
            EventType::Vehicle => "vehicle",
            EventType::Audio => "audio",
            EventType::Tampering => "tampering",
        }
    }

    /// Material icon for the type column.
    pub fn icon(self) -> &'static str {
        match self {
            EventType::Motion => "directions_run",
            EventType::People => "person",
            EventType::Vehicle => "directions_car",
            EventType::Audio => "hearing",
            EventType::Tampering => "report",
        }
    }
}

/// What the filter bar holds for one page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterState {
    /// Active quick-filter pill; `None` while an advanced range is used.
    pub preset: Option<Preset>,
    /// Advanced From / To text (`2026-10-11 00:00`); used when `preset` is
    /// `None`.
    pub from: String,
    pub to: String,
    /// Camera filter (mirrors `AppState::filter_camera`; `None` = all).
    pub camera: Option<i64>,
    /// Events page only.
    pub event_type: Option<EventType>,
    pub advanced_open: bool,
    /// 0-based page number.
    pub page_no: usize,
}

impl Default for FilterState {
    fn default() -> Self {
        Self {
            preset: Some(Preset::Today),
            from: String::new(),
            to: String::new(),
            camera: None,
            event_type: None,
            advanced_open: false,
            page_no: 0,
        }
    }
}

impl FilterState {
    /// Effective time range for `now`: the preset, else the advanced
    /// fields (an empty To means "until now", an empty From means "since
    /// the epoch"), else `None` when neither is usable.
    pub fn time_range(&self, now: NaiveDateTime) -> Option<(NaiveDateTime, NaiveDateTime)> {
        if let Some(p) = self.preset {
            return Some(p.range(now));
        }
        let from = parse_local(&self.from);
        let to = parse_local(&self.to);
        match (from, to) {
            (None, None) => None,
            (Some(f), None) => Some((f, now)),
            (None, Some(t)) => Some((chrono::DateTime::UNIX_EPOCH.naive_utc(), t)),
            (Some(f), Some(t)) => Some((f.min(t), f.max(t))),
        }
    }

    /// `Today`, `2026-10-11 00:00 → 2026-10-12 00:00`, or `Any time`.
    pub fn time_label(&self) -> String {
        if let Some(p) = self.preset {
            return p.label().to_string();
        }
        match (self.from.trim(), self.to.trim()) {
            ("", "") => "Any time".to_string(),
            (f, "") => format!("since {f}"),
            ("", t) => format!("until {t}"),
            (f, t) => format!("{f} → {t}"),
        }
    }

    /// Summary row text: `Time: Today  ·  Camera: CAM 59  ·  Type: Motion`.
    pub fn summary(&self, camera_name: Option<&str>) -> String {
        let mut parts = vec![format!("Time: {}", self.time_label())];
        parts.push(format!("Camera: {}", camera_name.unwrap_or("All cameras")));
        if let Some(t) = self.event_type {
            parts.push(format!("Type: {}", t.label()));
        }
        parts.join("  ·  ")
    }

    /// Back to the defaults (the camera filter is owned by the store and
    /// cleared by the caller).
    pub fn reset(&mut self) {
        let camera = self.camera;
        *self = FilterState {
            camera,
            ..FilterState::default()
        };
    }
}

// ---------------------------------------------------------------------------
// Pure formatting helpers
// ---------------------------------------------------------------------------

/// Parse `YYYY-MM-DD HH:MM[:SS]` or `YYYY-MM-DD` (local wall-clock).
pub fn parse_local(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M"))
        .ok()
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
        })
}

/// Local wall-clock → unix seconds (ambiguous DST instants take the
/// earlier one).
pub fn to_unix(t: NaiveDateTime) -> i64 {
    Local
        .from_local_datetime(&t)
        .earliest()
        .map(|d| d.timestamp())
        .unwrap_or_else(|| t.and_utc().timestamp())
}

/// Unix seconds → local wall-clock.
pub fn from_unix(ts: i64) -> NaiveDateTime {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.naive_local())
        .unwrap_or_else(|| chrono::DateTime::UNIX_EPOCH.naive_utc())
}

/// `2026-10-11 15:03:00`.
pub fn format_local(ts: i64) -> String {
    from_unix(ts).format("%Y-%m-%d %H:%M:%S").to_string()
}

/// `2026-10-11 15:03:00 – 15:27:50`; the end keeps its date when it is on
/// another day. `sep` is ` – ` for recordings and ` → ` for time lapse.
pub fn format_range_naive(start: NaiveDateTime, stop: NaiveDateTime, sep: &str) -> String {
    let a = start.format("%Y-%m-%d %H:%M:%S");
    if start.date() == stop.date() {
        format!("{a}{sep}{}", stop.format("%H:%M:%S"))
    } else {
        format!("{a}{sep}{}", stop.format("%Y-%m-%d %H:%M:%S"))
    }
}

/// [`format_range_naive`] on unix seconds.
pub fn format_range(start: i64, stop: i64, sep: &str) -> String {
    format_range_naive(from_unix(start), from_unix(stop), sep)
}

/// `412 MB`, `1.2 GB`, `980 KB`.
pub fn format_size(bytes: i64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes.max(0) as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.0} MB", b / (KB * KB))
    } else {
        format!("{:.1} GB", b / (KB * KB * KB))
    }
}

/// `24:50` or `1:02:03`.
pub fn format_duration(secs: i64) -> String {
    let s = secs.max(0);
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// `Recording.List`'s `type` → the mode word of the meta line.
pub fn recording_mode(event_type: i64) -> &'static str {
    match event_type {
        0 => "continuous",
        1 => "motion",
        2 => "alarm",
        3 => "manual",
        4 => "custom",
        5 => "action rule",
        6 => "edge",
        _ => "recording",
    }
}

/// A file-system-safe version of a camera or task name.
pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "camera".to_string()
    } else {
        trimmed.to_string()
    }
}

/// `<name>_<YYYYmmdd_HHMMSS>.<ext>`
pub fn media_filename(name: &str, ts: i64, ext: &str) -> String {
    format!(
        "{}_{}.{ext}",
        sanitize_filename(name),
        from_unix(ts).format("%Y%m%d_%H%M%S")
    )
}

/// Where downloads go: the sibling `Downloads` of the configured
/// `snapshot_dir` (`<data_dir>/Downloads`), else `~/Downloads`.
pub fn download_dir(store: &Store) -> PathBuf {
    let snapshot_dir = store.config().general.snapshot_dir.clone();
    if !snapshot_dir.is_empty()
        && let Some(parent) = Path::new(&snapshot_dir).parent()
        && !parent.as_os_str().is_empty()
    {
        return parent.join("Downloads");
    }
    glib::user_special_dir(glib::UserDirectory::Downloads)
        .unwrap_or_else(|| glib::home_dir().join("Downloads"))
}

/// Where player snapshots go (`general.snapshot_dir`).
pub fn snapshot_dir(store: &Store) -> PathBuf {
    let dir = store.config().general.snapshot_dir.clone();
    if dir.is_empty() {
        glib::user_special_dir(glib::UserDirectory::Pictures)
            .unwrap_or_else(|| glib::home_dir().join("Pictures"))
    } else {
        PathBuf::from(dir)
    }
}

/// The toplevel window holding `w`, for dialogs.
pub fn parent_window(w: &impl IsA<gtk::Widget>) -> Option<gtk::Window> {
    w.as_ref().root().and_downcast::<gtk::Window>()
}

// ---------------------------------------------------------------------------
// Downloads
// ---------------------------------------------------------------------------

/// What to download.
#[derive(Clone, Debug)]
pub enum DownloadKind {
    Recording(i64),
    /// A time lapse recording: `Recording.Download` with `recEvtType=3`.
    TimeLapse(i64),
    Snapshot(i64),
}

/// A `tokio::fs::File` that reports progress every few MiB.
struct ProgressWriter {
    inner: tokio::fs::File,
    written: u64,
    reported: u64,
    tx: async_channel::Sender<u64>,
}

impl tokio::io::AsyncWrite for ProgressWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &r {
            self.written += *n as u64;
            if self.written - self.reported >= 4 * 1024 * 1024 {
                self.reported = self.written;
                let _ = self.tx.try_send(self.written);
            }
        }
        r
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Save `kind` as `filename` under [`download_dir`], toasting progress and
/// completion.
pub fn start_download(
    store: &Rc<Store>,
    services: &Rc<Services>,
    kind: DownloadKind,
    filename: String,
) {
    let Some(client) = services.client() else {
        store.toast("Not connected");
        return;
    };
    let dir = download_dir(store);
    let path = dir.join(&filename);
    store.toast(format!("Downloading {filename}…"));
    let (tx, rx) = async_channel::bounded::<u64>(8);
    let fut = services.run(async move {
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        let mut out = ProgressWriter {
            inner: file,
            written: 0,
            reported: 0,
            tx,
        };
        let result = match kind {
            DownloadKind::Recording(id) => client.download_recording(id, &mut out).await,
            DownloadKind::TimeLapse(id) => {
                let req = ApiRequest::new(
                    "SYNO.SurveillanceStation.Recording",
                    "Download",
                    ss_api::RECORDING_DOWNLOAD_VERSION,
                )
                .param("id", id)
                .param("recEvtType", 3);
                client.download_to(&req, &mut out).await
            }
            DownloadKind::Snapshot(id) => client.download_snapshot(id, &mut out).await,
        };
        match result {
            Ok(n) => Ok((n, path)),
            Err(e) => {
                let _ = tokio::fs::remove_file(&path).await;
                Err(e.to_string())
            }
        }
    });
    let store2 = store.clone();
    let store = store.clone();
    let name = filename.clone();
    glib::spawn_future_local(async move {
        while let Ok(n) = rx.recv().await {
            store.toast(format!("Downloading {name} · {}", format_size(n as i64)));
        }
    });
    glib::spawn_future_local(async move {
        match fut.await {
            Ok((n, path)) => store2.toast(format!(
                "Saved {} ({}) to {}",
                filename,
                format_size(n as i64),
                path.parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            )),
            Err(e) => store2.toast(format!("Download failed: {e}")),
        }
    });
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

type LoadedListener = Box<dyn Fn(i64)>;
type FilterListener = Box<dyn Fn()>;

/// One browse page's state and reload plumbing, shared by the filter bar,
/// the content and the pagination footer.
pub struct Browse {
    pub store: Rc<Store>,
    pub services: Rc<Services>,
    pub page: Page,
    /// Rows per page (`usize::MAX` = unpaginated).
    pub page_size: usize,
    pub filter: RefCell<FilterState>,
    loader: RefCell<Option<Rc<dyn Fn()>>>,
    on_loaded: RefCell<Vec<LoadedListener>>,
    on_filter: RefCell<Vec<FilterListener>>,
    generation: Cell<u64>,
    dirty: Cell<bool>,
    loaded_once: Cell<bool>,
    /// Set when the next load must hit the server even if a content module
    /// caches the full result set (Events pages client-side).
    force: Cell<bool>,
}

impl Browse {
    pub fn new(store: &Rc<Store>, services: &Rc<Services>, page: Page) -> Rc<Browse> {
        let page_size = match page {
            Page::Snapshots => 9,
            Page::TimeLapse => usize::MAX,
            _ => 8,
        };
        let mut filter = FilterState {
            camera: store.get().filter_camera,
            ..FilterState::default()
        };
        if let Some(sp) = search_page(page) {
            let profile = profile_name(store);
            let saved = store.config().search_filters(sp, &profile);
            let preset = saved.preset.and_then(Preset::from_config);
            if preset.is_some() {
                filter.preset = preset;
            } else if !saved.from_time.is_empty() || !saved.to_time.is_empty() {
                filter.preset = None;
                filter.from = saved.from_time;
                filter.to = saved.to_time;
                filter.advanced_open = true;
            }
        }
        Rc::new(Browse {
            store: store.clone(),
            services: services.clone(),
            page,
            page_size,
            filter: RefCell::new(filter),
            loader: RefCell::new(None),
            on_loaded: RefCell::new(Vec::new()),
            on_filter: RefCell::new(Vec::new()),
            generation: Cell::new(0),
            dirty: Cell::new(true),
            loaded_once: Cell::new(false),
            force: Cell::new(true),
        })
    }

    /// Reload from the server, bypassing any client-side cache.
    pub fn refresh(&self) {
        self.force.set(true);
        self.reload();
    }

    /// Whether the current load must refetch; clears the flag.
    pub fn take_force(&self) -> bool {
        self.force.replace(false)
    }

    pub fn client(&self) -> Option<Client> {
        self.services.client()
    }

    /// This page is the one on the stage and we are logged in.
    pub fn visible(&self) -> bool {
        let s = self.store.get();
        s.logged_in() && s.page == self.page
    }

    pub fn toast(&self, text: impl Into<String>) {
        self.store.toast(text);
    }

    pub fn set_loader(&self, f: Rc<dyn Fn()>) {
        *self.loader.borrow_mut() = Some(f);
    }

    /// Called with the total once a load finished (pagination, summary).
    pub fn on_loaded(&self, f: impl Fn(i64) + 'static) {
        self.on_loaded.borrow_mut().push(Box::new(f));
    }

    /// Called whenever the filter changed (bar re-sync, camera chips).
    pub fn on_filter(&self, f: impl Fn() + 'static) {
        self.on_filter.borrow_mut().push(Box::new(f));
    }

    pub fn notify_filter(&self) {
        let n = self.on_filter.borrow().len();
        for i in 0..n {
            let fs = self.on_filter.borrow();
            if let Some(f) = fs.get(i) {
                let f = f as *const FilterListener;
                drop(fs);
                // SAFETY: listeners are only appended, never removed, and
                // `Browse` outlives every callback invocation.
                unsafe { (*f)() };
            }
        }
    }

    /// A filter value changed: back to page 1, persist, notify, reload.
    pub fn filter_changed(self: &Rc<Self>) {
        self.filter.borrow_mut().page_no = 0;
        self.force.set(true);
        self.persist();
        self.notify_filter();
        self.request_reload();
    }

    /// Reload now if visible, else when the page is shown next.
    pub fn request_reload(self: &Rc<Self>) {
        if self.visible() {
            self.reload();
        } else {
            self.dirty.set(true);
        }
    }

    pub fn reload(&self) {
        self.dirty.set(false);
        self.loaded_once.set(true);
        self.generation.set(self.generation.get() + 1);
        let loader = self.loader.borrow().clone();
        if let Some(f) = loader {
            f();
        }
    }

    /// Token for the current load; results of older loads are dropped.
    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.generation.get() == generation
    }

    /// Content finished loading `total` items.
    pub fn loaded(&self, total: i64) {
        for f in self.on_loaded.borrow().iter() {
            f(total);
        }
    }

    pub fn set_page(self: &Rc<Self>, page_no: usize) {
        self.filter.borrow_mut().page_no = page_no;
        self.reload();
    }

    /// Camera filter changes go through the store so the camera panel
    /// follows; the store subscription then calls [`Browse::filter_changed`].
    pub fn set_camera(self: &Rc<Self>, camera: Option<i64>) {
        if self.store.get().filter_camera == camera {
            return;
        }
        self.store
            .update(Change::FilterCamera, |s| s.filter_camera = camera);
    }

    /// Effective `[from, to]` in unix seconds.
    pub fn time_range_unix(&self) -> Option<(i64, i64)> {
        self.filter
            .borrow()
            .time_range(Local::now().naive_local())
            .map(|(a, b)| (to_unix(a), to_unix(b)))
    }

    /// The filtered camera, else every known camera.
    pub fn camera_ids(&self) -> Vec<i64> {
        match self.filter.borrow().camera {
            Some(id) => vec![id],
            None => self.store.get().cameras.iter().map(|c| c.id).collect(),
        }
    }

    pub fn camera_name(&self, id: i64) -> Option<String> {
        self.store.get().camera(id).map(|c| c.name.clone())
    }

    /// Summary row text for the current filter.
    pub fn summary(&self) -> String {
        let f = self.filter.borrow();
        let name = f.camera.and_then(|id| self.camera_name(id));
        f.summary(name.as_deref())
    }

    /// Offset of the current page.
    pub fn offset(&self) -> u32 {
        if self.page_size == usize::MAX {
            0
        } else {
            (self.filter.borrow().page_no * self.page_size) as u32
        }
    }

    fn persist(&self) {
        let Some(sp) = search_page(self.page) else {
            return;
        };
        let profile = profile_name(&self.store);
        let f = self.filter.borrow();
        let filters = ss_config::SearchFilters {
            camera_ids: f.camera.map(|c| c.max(0) as u32).into_iter().collect(),
            from_time: if f.preset.is_some() {
                String::new()
            } else {
                f.from.clone()
            },
            to_time: if f.preset.is_some() {
                String::new()
            } else {
                f.to.clone()
            },
            preset: f.preset.map(Preset::to_config),
        };
        drop(f);
        self.store
            .config()
            .set_search_filters(sp, &profile, &filters);
        self.store.save_soon();
    }
}

fn search_page(page: Page) -> Option<ss_config::SearchPage> {
    match page {
        Page::Recordings => Some(ss_config::SearchPage::Recordings),
        Page::Events => Some(ss_config::SearchPage::Events),
        Page::Snapshots => Some(ss_config::SearchPage::Snapshots),
        _ => None,
    }
}

fn profile_name(store: &Store) -> String {
    store
        .get()
        .connection
        .as_ref()
        .map(|c| c.profile.clone())
        .unwrap_or_else(|| store.config().general.default_profile.clone())
}

/// A page's content: the widget and how to (re)load it from the filter.
pub struct Content {
    pub widget: gtk::Widget,
    pub load: Rc<dyn Fn()>,
}

/// Muted centred hint for empty / loading / error states.
pub fn hint(text: &str) -> gtk::Widget {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("browse-hint");
    l.set_halign(gtk::Align::Center);
    l.set_valign(gtk::Align::Center);
    l.set_vexpand(true);
    l.set_wrap(true);
    l.set_justify(gtk::Justification::Center);
    l.upcast()
}

/// Remove every child of a box.
pub fn clear_box(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

/// Build one browse page: filter bar / content / pagination footer.
pub fn build(store: &Rc<Store>, services: &Rc<Services>, page: Page) -> gtk::Widget {
    let ctx = Browse::new(store, services, page);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("browse");

    let bar = filter_bar::build(&ctx);
    root.append(&bar);

    let content = match page {
        Page::Recordings => recordings::build(&ctx),
        Page::Events => events::build(&ctx),
        Page::Snapshots => snapshots::build(&ctx),
        _ => timelapse::build(&ctx),
    };
    content.widget.set_vexpand(true);
    root.append(&content.widget);
    ctx.set_loader(content.load);

    root.append(&pagination::build(&ctx));

    let weak: Weak<Browse> = Rc::downgrade(&ctx);
    store.subscribe(move |change| {
        let Some(ctx) = weak.upgrade() else { return };
        match change {
            Change::FilterCamera => {
                let cam = ctx.store.get().filter_camera;
                if ctx.filter.borrow().camera != cam {
                    ctx.filter.borrow_mut().camera = cam;
                    ctx.filter_changed();
                }
            }
            Change::Page => {
                if ctx.visible() && (ctx.dirty.get() || !ctx.loaded_once.get()) {
                    ctx.reload();
                }
            }
            Change::Connection => {
                ctx.force.set(true);
                if ctx.store.get().logged_in() {
                    ctx.dirty.set(true);
                    ctx.request_reload();
                } else {
                    ctx.loaded_once.set(false);
                    ctx.dirty.set(true);
                    ctx.reload();
                }
            }
            Change::Cameras => {
                ctx.notify_filter();
                if ctx.visible() && !ctx.loaded_once.get() {
                    ctx.reload();
                }
            }
            _ => {}
        }
    });

    root.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
    }

    #[test]
    fn preset_ranges() {
        let now = at(2026, 10, 11, 15, 27);
        assert_eq!(
            Preset::Today.range(now),
            (at(2026, 10, 11, 0, 0), at(2026, 10, 12, 0, 0))
        );
        assert_eq!(
            Preset::Yesterday.range(now),
            (at(2026, 10, 10, 0, 0), at(2026, 10, 11, 0, 0))
        );
        assert_eq!(Preset::Last24h.range(now), (at(2026, 10, 10, 15, 27), now));
        assert_eq!(Preset::Last7d.range(now), (at(2026, 10, 4, 15, 27), now));
    }

    #[test]
    fn advanced_range_and_fallbacks() {
        let now = at(2026, 10, 11, 15, 27);
        let mut f = FilterState {
            preset: None,
            from: "2026-10-01 08:00".into(),
            to: "2026-10-02".into(),
            ..FilterState::default()
        };
        assert_eq!(
            f.time_range(now),
            Some((at(2026, 10, 1, 8, 0), at(2026, 10, 2, 0, 0)))
        );
        // Reversed bounds are swapped.
        f.from = "2026-10-05 00:00".into();
        f.to = "2026-10-01 00:00".into();
        assert_eq!(
            f.time_range(now),
            Some((at(2026, 10, 1, 0, 0), at(2026, 10, 5, 0, 0)))
        );
        // Only From: until now.
        f.to.clear();
        assert_eq!(f.time_range(now), Some((at(2026, 10, 5, 0, 0), now)));
        // Nothing usable.
        f.from = "garbage".into();
        assert_eq!(f.time_range(now), None);
        // Preset wins over the fields.
        f.preset = Some(Preset::Today);
        assert_eq!(f.time_range(now), Some(Preset::Today.range(now)));
    }

    #[test]
    fn summary_text() {
        let mut f = FilterState::default();
        assert_eq!(f.summary(None), "Time: Today  ·  Camera: All cameras");
        f.event_type = Some(EventType::Motion);
        assert_eq!(
            f.summary(Some("CAM 59 - RLC823A")),
            "Time: Today  ·  Camera: CAM 59 - RLC823A  ·  Type: Motion"
        );
        f.preset = None;
        f.from = "2026-10-11 00:00".into();
        f.to = "2026-10-12 00:00".into();
        f.event_type = None;
        assert_eq!(
            f.summary(None),
            "Time: 2026-10-11 00:00 → 2026-10-12 00:00  ·  Camera: All cameras"
        );
        f.from.clear();
        f.to.clear();
        assert_eq!(f.time_label(), "Any time");
    }

    #[test]
    fn reset_keeps_camera() {
        let mut f = FilterState {
            preset: None,
            from: "x".into(),
            to: "y".into(),
            camera: Some(59),
            event_type: Some(EventType::Audio),
            advanced_open: true,
            page_no: 3,
        };
        f.reset();
        assert_eq!(
            f,
            FilterState {
                camera: Some(59),
                ..FilterState::default()
            }
        );
    }

    #[test]
    fn formatting() {
        assert_eq!(format_size(412 * 1024 * 1024), "412 MB");
        assert_eq!(format_size(1536), "2 KB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
        assert_eq!(format_size(12), "12 B");
        assert_eq!(format_duration(24 * 60 + 50), "24:50");
        assert_eq!(format_duration(3723), "1:02:03");
        assert_eq!(format_duration(-5), "0:00");
        assert_eq!(
            format_range_naive(at(2026, 10, 11, 15, 3), at(2026, 10, 11, 15, 27), " – "),
            "2026-10-11 15:03:00 – 15:27:00"
        );
        assert_eq!(
            format_range_naive(at(2026, 10, 10, 6, 0), at(2026, 10, 11, 20, 0), " → "),
            "2026-10-10 06:00:00 → 2026-10-11 20:00:00"
        );
        assert_eq!(recording_mode(0), "continuous");
        assert_eq!(recording_mode(1), "motion");
        assert_eq!(sanitize_filename("CAM 59 - RLC823A"), "CAM_59_-_RLC823A");
        assert_eq!(sanitize_filename("///"), "camera");
    }

    #[test]
    fn parse_local_formats() {
        assert_eq!(
            parse_local("2026-10-11 00:00"),
            Some(at(2026, 10, 11, 0, 0))
        );
        assert_eq!(
            parse_local(" 2026-10-11 07:05:00 "),
            Some(at(2026, 10, 11, 7, 5))
        );
        assert_eq!(parse_local("2026-10-11"), Some(at(2026, 10, 11, 0, 0)));
        assert_eq!(parse_local(""), None);
        assert_eq!(parse_local("11/10/2026"), None);
        // Round trip through unix seconds in the local zone.
        let t = at(2026, 3, 5, 12, 30);
        assert_eq!(from_unix(to_unix(t)), t);
    }
}
