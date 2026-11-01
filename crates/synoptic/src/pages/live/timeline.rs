//! The multi-lane timeline under the Live View grid.
//!
//! A 44 px control row (readout, filter / download / date / zoom, tracking,
//! transport, speed, Live) over one Cairo `DrawingArea`: a name column, a
//! ruler with ticks, and one lane per camera in the layout showing
//! recording presence as bars with event markers (motion orange, alarm
//! red, manual/other purple) drawn on top, the live edge and the playhead
//! (red live, blue history) spanning every lane.
//!
//! Data comes per hour-aligned chunk from `RecordingPicker.EnumInterval`
//! and `EventCenter.Event.List`, falling back to a paged `Recording.List`
//! for either when the NAS lacks the API or it fails; chunks that covered
//! "now" when fetched are refreshed after 60 s.
//!
//! Pure geometry / time math lives at the bottom and is unit-tested; the
//! widget code above it only maps gestures to those functions and to the
//! grid's playback controller.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::{Rc, Weak};

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use gtk::cairo;
use gtk::prelude::*;
use gtk::{gdk, glib};

use super::grid::{Grid, Mode, Playback, SPEEDS};
use crate::icons;
use crate::services::Services;
use crate::state::{Change, Store, Theme};
use crate::widgets::controls::{divider_v, label, set_class, spacer};

// --- constants ------------------------------------------------------------------

/// The live edge sits at 97 % of the ruler width in follow-live mode.
pub const LIVE_EDGE_FRAC: f64 = 0.97;
/// Position ≥ now − window × 3 % counts as live.
pub const LIVE_TOLERANCE_FRAC: f64 = 0.03;
/// Target number of tick marks.
pub const TICK_COUNT: usize = 12;
/// Zoom levels (window length in seconds), 5 min … 24 h; index 4 is the default 2 h.
pub const ZOOM_LEVELS: [i64; 9] = [300, 900, 1800, 3600, 7200, 14400, 28800, 43200, 86400];
pub const DEFAULT_ZOOM: usize = 4;
/// Data cache granularity (s): one hour.
pub const CHUNK_SECS: i64 = 3600;
/// Chunks that covered "now" when fetched are stale after this long.
pub const REFRESH_SECS: i64 = 60;
/// Most chunks one request covers (24 h).
pub const MAX_CHUNKS_PER_FETCH: i64 = 24;
/// Oldest time the timeline can be panned to (400 days).
const MAX_HISTORY_SECS: i64 = 400 * 86400;

/// Name column width.
const NAME_W: f64 = 112.0;
/// Ruler (ticks + labels) height.
const RULER_H: f64 = 22.0;
/// Bottom padding under the last lane.
const PAD_BOTTOM: f64 = 6.0;
/// Control row height.
const CONTROLS_H: i32 = 44;
/// Minimum total strip height.
const MIN_HEIGHT: i32 = 112;
/// Pixels around the playhead that start a scrub instead of a pan.
const SCRUB_GRAB_PX: f64 = 7.0;
/// Drags shorter than this are clicks.
const CLICK_SLOP_PX: f64 = 3.0;

// --- event kinds ----------------------------------------------------------------

/// What an event marker is, for colour, label and the filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Motion,
    Alarm,
    Manual,
    Other,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Motion, Kind::Alarm, Kind::Manual, Kind::Other];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Motion => "Motion",
            Kind::Alarm => "Alarm",
            Kind::Manual => "Manual",
            Kind::Other => "Other",
        }
    }

    /// `config.toml` key (`session.timeline_event_types`).
    pub fn key(self) -> &'static str {
        match self {
            Kind::Motion => "motion",
            Kind::Alarm => "alarm",
            Kind::Manual => "manual",
            Kind::Other => "other",
        }
    }

    pub fn from_key(key: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.key() == key)
    }

    /// Legacy `Recording.List` / `Event.List` mode: 1 motion, 2 alarm, 3 manual.
    pub fn from_recording_mode(mode: i64) -> Kind {
        match mode {
            1 => Kind::Motion,
            2 => Kind::Alarm,
            3 => Kind::Manual,
            _ => Kind::Other,
        }
    }

    /// `EventCenter.Event.List` `event_type`: 10 motion; 12 tampering, 18
    /// audio and 19 digital input count as alarms; AI object detection
    /// (11) and the rest are "other".
    pub fn from_event_center(event_type: i64) -> Kind {
        match event_type {
            10 => Kind::Motion,
            12 | 18 | 19 => Kind::Alarm,
            _ => Kind::Other,
        }
    }
}

/// One event marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventMark {
    pub camera_id: i64,
    pub kind: Kind,
    /// Unix seconds; `end > start` (at least 1 s).
    pub start: i64,
    pub end: i64,
}

/// Which kinds (and whether recordings) are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Filter {
    pub kinds: [bool; 4],
    pub recordings: bool,
}

impl Filter {
    #[cfg(test)]
    pub fn all() -> Filter {
        Filter {
            kinds: [true; 4],
            recordings: true,
        }
    }

    pub fn shows(&self, kind: Kind) -> bool {
        self.kinds[kind as usize]
    }

    pub fn set(&mut self, kind: Kind, on: bool) {
        self.kinds[kind as usize] = on;
    }

    pub fn from_config(session: &ss_config::Session) -> Filter {
        let mut f = Filter {
            kinds: [false; 4],
            recordings: session.timeline_show_recordings,
        };
        for key in &session.timeline_event_types {
            if let Some(k) = Kind::from_key(key) {
                f.set(k, true);
            }
        }
        f
    }

    pub fn write_config(&self, session: &mut ss_config::Session) {
        session.timeline_event_types = Kind::ALL
            .into_iter()
            .filter(|k| self.shows(*k))
            .map(|k| k.key().to_string())
            .collect();
        session.timeline_show_recordings = self.recordings;
    }
}

// --- data cache -----------------------------------------------------------------

/// What one fetch returned for `[from, to)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fetched {
    /// camera id → merged recording spans (unix s)
    pub recordings: HashMap<i64, Vec<(i64, i64)>>,
    pub events: Vec<EventMark>,
}

#[derive(Clone, Debug, Default)]
struct Chunk {
    recordings: HashMap<i64, Vec<(i64, i64)>>,
    events: Vec<EventMark>,
    /// Unix seconds when fetched.
    fetched_at: i64,
}

#[derive(Default)]
pub struct Cache {
    /// Sorted camera ids the chunks were fetched for.
    ids: Vec<i64>,
    chunks: BTreeMap<i64, Chunk>,
}

impl Cache {
    fn reset_for(&mut self, ids: &[i64]) {
        if self.ids != ids {
            self.ids = ids.to_vec();
            self.chunks.clear();
        }
    }

    /// Chunk starts in `[from, to]` (clipped to `now`) that are missing or stale.
    pub fn missing(&self, from: i64, to: i64, now: i64) -> Vec<i64> {
        let first = from.div_euclid(CHUNK_SECS) * CHUNK_SECS;
        let last = to.min(now).div_euclid(CHUNK_SECS) * CHUNK_SECS;
        let mut out = Vec::new();
        let mut c = first;
        while c <= last {
            match self.chunks.get(&c) {
                None => out.push(c),
                Some(chunk) => {
                    let covered_now = c + CHUNK_SECS > chunk.fetched_at;
                    if covered_now && now - chunk.fetched_at >= REFRESH_SECS {
                        out.push(c);
                    }
                }
            }
            c += CHUNK_SECS;
        }
        out
    }

    /// Store `fetched` for the chunks in `[from, to)`.
    pub fn store(&mut self, from: i64, to: i64, fetched: &Fetched, now: i64) {
        let mut c = from;
        while c < to {
            let end = c + CHUNK_SECS;
            let mut chunk = Chunk {
                fetched_at: now,
                ..Default::default()
            };
            for (id, spans) in &fetched.recordings {
                let v: Vec<(i64, i64)> = spans
                    .iter()
                    .copied()
                    .filter(|&(a, b)| b > c && a < end)
                    .collect();
                if !v.is_empty() {
                    chunk.recordings.insert(*id, v);
                }
            }
            chunk.events = fetched
                .events
                .iter()
                .filter(|e| e.start >= c && e.start < end)
                .cloned()
                .collect();
            self.chunks.insert(c, chunk);
            c = end;
        }
    }

    /// Merged recording spans of `camera_id` overlapping `[from, to]`.
    pub fn recordings(&self, camera_id: i64, from: i64, to: i64) -> Vec<(i64, i64)> {
        let first = from.div_euclid(CHUNK_SECS) * CHUNK_SECS;
        let spans = self
            .chunks
            .range(first..=to)
            .flat_map(|(_, c)| c.recordings.get(&camera_id).into_iter().flatten())
            .copied()
            .filter(|&(a, b)| b >= from && a <= to);
        ss_api::merge_intervals(spans)
    }

    /// Events (any camera) starting in `[from, to]`, in start order.
    pub fn events(&self, from: i64, to: i64) -> Vec<EventMark> {
        let first = from.div_euclid(CHUNK_SECS) * CHUNK_SECS;
        let mut v: Vec<EventMark> = self
            .chunks
            .range(first..=to)
            .flat_map(|(_, c)| c.events.iter())
            .filter(|e| e.start >= from && e.start <= to)
            .cloned()
            .collect();
        v.sort_by_key(|e| (e.start, e.camera_id));
        v
    }

    /// Loaded range `(first chunk start, last chunk end)`.
    pub fn span(&self) -> Option<(i64, i64)> {
        let first = *self.chunks.keys().next()?;
        let last = *self.chunks.keys().next_back()?;
        Some((first, last + CHUNK_SECS))
    }

    pub fn event_count(&self) -> usize {
        self.chunks.values().map(|c| c.events.len()).sum()
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
}

/// Presence + events for `camera_ids` in `[from, to)`: `EnumInterval` and
/// `EventCenter.Event.List` where the NAS has them and they answer, else a
/// paged `Recording.List` (its `type` becomes the event kind).
pub async fn fetch_range(
    client: ss_api::Client,
    camera_ids: Vec<i64>,
    from: i64,
    to: i64,
) -> Fetched {
    let mut recordings: Option<HashMap<i64, Vec<(i64, i64)>>> = None;
    let mut events: Option<Vec<EventMark>> = None;

    if client.has_api(ss_api::event::RECORDING_PICKER_API) {
        match client.enum_interval(&camera_ids, from, to).await {
            Ok(cams) => {
                let mut map: HashMap<i64, Vec<(i64, i64)>> = HashMap::new();
                for c in cams {
                    map.entry(c.camera_id)
                        .or_default()
                        .extend(c.presence_spans());
                }
                for v in map.values_mut() {
                    *v = ss_api::merge_intervals(v.drain(..));
                }
                recordings = Some(map);
            }
            Err(e) => tracing::warn!("EnumInterval failed, falling back to Recording.List: {e}"),
        }
    }
    if client.has_api(ss_api::event::EVENT_CENTER_API) {
        match client.list_event_center_events(&camera_ids, from, to).await {
            Ok(evs) => {
                events = Some(
                    evs.iter()
                        .map(|e| EventMark {
                            camera_id: e.camera_id,
                            kind: Kind::from_event_center(e.event_type),
                            start: e.start_time,
                            end: e.start_time + e.duration.max(1),
                        })
                        .collect(),
                );
            }
            Err(e) => {
                tracing::warn!("EventCenter list failed, deriving events from recordings: {e}")
            }
        }
    }
    if recordings.is_none() || events.is_none() {
        let mut listed: Vec<ss_api::Recording> = Vec::new();
        for &id in &camera_ids {
            let filter = ss_api::RecordingFilter::camera(id)
                .between(from, to)
                .page(0, 500);
            match client.list_recordings_all(&filter).await {
                Ok(v) => listed.extend(v),
                Err(e) => tracing::warn!("Recording.List for camera {id}: {e}"),
            }
        }
        if recordings.is_none() {
            let mut map: HashMap<i64, Vec<(i64, i64)>> = HashMap::new();
            for r in &listed {
                map.entry(r.camera_id)
                    .or_default()
                    .push((r.start_time, r.stop_time.max(r.start_time + 1)));
            }
            for v in map.values_mut() {
                *v = ss_api::merge_intervals(v.drain(..));
            }
            recordings = Some(map);
        }
        if events.is_none() {
            events = Some(
                listed
                    .iter()
                    .filter(|r| matches!(r.event_type, 1..=3))
                    .map(|r| EventMark {
                        camera_id: r.camera_id,
                        kind: Kind::from_recording_mode(r.event_type),
                        start: r.start_time,
                        end: r.stop_time.max(r.start_time + 1),
                    })
                    .collect(),
            );
        }
    }
    Fetched {
        recordings: recordings.unwrap_or_default(),
        events: events.unwrap_or_default(),
    }
}

// --- widget ------------------------------------------------------------------------

enum Drag {
    Pan { start_view: i64 },
    Scrub { time: i64 },
}

/// Pixel geometry of the last draw, for hit testing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Geometry {
    pub width: f64,
    pub height: f64,
    pub name_w: f64,
    pub ruler_h: f64,
    pub lane_h: f64,
    pub lanes: usize,
}

impl Geometry {
    /// Width of the time axis.
    pub fn axis_w(&self) -> f64 {
        (self.width - self.name_w).max(1.0)
    }

    pub fn axis_x(&self, frac: f64) -> f64 {
        self.name_w + frac * self.axis_w()
    }

    pub fn frac_at(&self, x: f64) -> f64 {
        (x - self.name_w) / self.axis_w()
    }

    pub fn lane_y(&self, lane: usize) -> f64 {
        self.ruler_h + lane as f64 * self.lane_h
    }

    /// Lane under `y`, if any.
    pub fn lane_at(&self, y: f64) -> Option<usize> {
        if y < self.ruler_h || self.lane_h <= 0.0 {
            return None;
        }
        let i = ((y - self.ruler_h) / self.lane_h).floor();
        (i >= 0.0 && (i as usize) < self.lanes).then_some(i as usize)
    }
}

struct Timeline {
    store: Rc<Store>,
    services: Rc<Services>,
    grid: Rc<Grid>,
    root: gtk::Box,
    readout: gtk::Label,
    date_label: gtk::Label,
    zoom_label: gtk::Label,
    tracking_box: gtk::Box,
    tracking_cam: gtk::Label,
    pause_button: gtk::Button,
    pause_icon: gtk::Image,
    speed_dropdown: gtk::DropDown,
    live_button: gtk::Button,
    live_label: gtk::Label,
    filter_checks: Vec<(Kind, gtk::CheckButton)>,
    recordings_check: gtk::CheckButton,
    calendar: gtk::Calendar,
    hour_spin: gtk::SpinButton,
    minute_spin: gtk::SpinButton,
    datetime_popover: gtk::Popover,
    area: gtk::DrawingArea,

    zoom: Cell<usize>,
    follow: Cell<bool>,
    view_start: Cell<i64>,
    hover: Cell<Option<(f64, f64)>>,
    drag: RefCell<Option<Drag>>,
    geometry: Cell<Geometry>,
    filter: Cell<Filter>,
    cache: RefCell<Cache>,
    fetching: Cell<bool>,
    fetch_generation: Cell<u64>,
    /// Last position shown in History (unix ms), kept while a seek is in flight.
    last_position_ms: Cell<Option<i64>>,
    /// `(mode, paused)` the controls were last refreshed for.
    shown_state: Cell<Option<(Mode, bool)>>,
    keys_installed: Cell<bool>,
    /// Weak handle to ourselves for async continuations.
    me: std::cell::OnceCell<Weak<Timeline>>,
}

thread_local! {
    /// The live page's timeline, for the headless driver.
    static DRIVER: RefCell<Option<Weak<Timeline>>> = const { RefCell::new(None) };
}

pub fn build(store: &Rc<Store>, services: &Rc<Services>, grid: &Rc<Grid>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("timeline");
    root.set_size_request(-1, MIN_HEIGHT);

    // ---- control row ----
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    controls.add_css_class("controls");
    controls.set_size_request(-1, CONTROLS_H);

    let readout_block = gtk::Box::new(gtk::Orientation::Vertical, 0);
    readout_block.set_size_request(150, -1);
    readout_block.set_valign(gtk::Align::Center);
    let readout = label("--:--:--", &["readout", "live"]);
    let date_label = label("", &["readout-date"]);
    readout_block.append(&readout);
    readout_block.append(&date_label);
    controls.append(&readout_block);

    // filter popover
    let filter_button = gtk::MenuButton::new();
    filter_button.add_css_class("icon-30");
    filter_button.set_child(Some(&icons::image("filter_list", 17)));
    filter_button.set_tooltip_text(Some("Filter events"));
    filter_button.set_valign(gtk::Align::Center);
    filter_button.set_widget_name("tl-filter");
    let filter_popover = gtk::Popover::new();
    filter_popover.set_has_arrow(false);
    filter_popover.add_css_class("tl-popover");
    let filter_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    filter_box.set_widget_name("tl-filter-box");
    filter_box.append(&label("Show on the timeline", &["tl-popover-title"]));
    let mut filter_checks = Vec::new();
    for kind in Kind::ALL {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let swatch = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        swatch.add_css_class("tl-swatch");
        swatch.add_css_class(&format!("tl-{}", kind.key()));
        swatch.set_valign(gtk::Align::Center);
        let check = gtk::CheckButton::with_label(kind.label());
        check.set_widget_name(&format!("tl-filter-{}", kind.key()));
        row.append(&swatch);
        row.append(&check);
        filter_box.append(&row);
        filter_checks.push((kind, check));
    }
    filter_box.append(&crate::widgets::controls::divider_h());
    let recordings_check = gtk::CheckButton::with_label("Show recordings");
    recordings_check.set_widget_name("tl-filter-recordings");
    filter_box.append(&recordings_check);
    filter_popover.set_child(Some(&filter_box));
    filter_button.set_popover(Some(&filter_popover));
    controls.append(&filter_button);

    let download_button = icons::bordered_button(
        "download",
        "icon-30",
        "Download the recording under the playhead",
    );
    download_button.set_widget_name("tl-download");
    controls.append(&download_button);

    // date / time popover
    let datetime_button = gtk::MenuButton::new();
    datetime_button.add_css_class("icon-30");
    datetime_button.set_child(Some(&icons::image("calendar_month", 17)));
    datetime_button.set_tooltip_text(Some("Go to date / time"));
    datetime_button.set_valign(gtk::Align::Center);
    datetime_button.set_widget_name("tl-datetime");
    let datetime_popover = gtk::Popover::new();
    datetime_popover.set_has_arrow(false);
    datetime_popover.add_css_class("tl-popover");
    let dt_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    dt_box.set_widget_name("tl-datetime-box");
    let calendar = gtk::Calendar::new();
    calendar.set_widget_name("tl-cal");
    dt_box.append(&calendar);
    let time_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    time_row.set_halign(gtk::Align::Center);
    let hour_spin = gtk::SpinButton::with_range(0.0, 23.0, 1.0);
    hour_spin.set_wrap(true);
    hour_spin.set_numeric(true);
    hour_spin.set_widget_name("tl-hour");
    let minute_spin = gtk::SpinButton::with_range(0.0, 59.0, 1.0);
    minute_spin.set_wrap(true);
    minute_spin.set_numeric(true);
    minute_spin.set_widget_name("tl-minute");
    time_row.append(&hour_spin);
    time_row.append(&label(":", &["tl-time-colon"]));
    time_row.append(&minute_spin);
    dt_box.append(&time_row);
    let dt_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let today_button = gtk::Button::with_label("Now");
    today_button.set_widget_name("tl-datetime-now");
    let go_button = gtk::Button::with_label("Go");
    go_button.add_css_class("primary");
    go_button.set_hexpand(true);
    go_button.set_widget_name("tl-datetime-go");
    dt_actions.append(&today_button);
    dt_actions.append(&go_button);
    dt_box.append(&dt_actions);
    datetime_popover.set_child(Some(&dt_box));
    datetime_button.set_popover(Some(&datetime_popover));
    controls.append(&datetime_button);

    let prev_day_button = icons::bordered_button("chevron_left", "icon-30", "Previous day");
    let next_day_button = icons::bordered_button("chevron_right", "icon-30", "Next day");
    let zoom_out_button = icons::bordered_button("zoom_out", "icon-30", "Zoom out (Ctrl + wheel)");
    let zoom_in_button = icons::bordered_button("zoom_in", "icon-30", "Zoom in (Ctrl + wheel)");
    for (b, name) in [
        (&prev_day_button, "tl-prev-day"),
        (&next_day_button, "tl-next-day"),
        (&zoom_out_button, "tl-zoom-out"),
        (&zoom_in_button, "tl-zoom-in"),
    ] {
        b.set_widget_name(name);
        controls.append(b);
    }
    let zoom_label = label("2 h", &["zoom-label"]);
    zoom_label.set_valign(gtk::Align::Center);
    zoom_label.set_width_chars(5);
    controls.append(&zoom_label);
    controls.append(&spacer());

    let tracking_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tracking_box.add_css_class("tracking");
    tracking_box.set_valign(gtk::Align::Center);
    tracking_box.append(&label("Tracking", &["tracking"]));
    let tracking_cam = label("", &["cam"]);
    tracking_cam.set_ellipsize(gtk::pango::EllipsizeMode::End);
    tracking_cam.set_max_width_chars(28);
    tracking_box.append(&tracking_cam);
    controls.append(&tracking_box);
    controls.append(&divider_v());

    let prev_button = icons::bordered_button("skip_previous", "icon-30", "Previous event");
    let back_button = icons::bordered_button("replay_10", "icon-30", "Back 10 s (←)");
    let pause_button = gtk::Button::new();
    pause_button.add_css_class("icon-30");
    pause_button.set_tooltip_text(Some("Pause (Space)"));
    pause_button.set_valign(gtk::Align::Center);
    let pause_icon = icons::image("pause", 17);
    pause_button.set_child(Some(&pause_icon));
    let forward_button = icons::bordered_button("forward_10", "icon-30", "Forward 10 s (→)");
    let next_button = icons::bordered_button("skip_next", "icon-30", "Next event");

    let labels: Vec<&str> = SPEEDS.iter().map(|(l, _)| *l).collect();
    let speed_dropdown = gtk::DropDown::from_strings(&labels);
    speed_dropdown.add_css_class("speed");
    speed_dropdown.set_valign(gtk::Align::Center);
    speed_dropdown.set_tooltip_text(Some("Playback speed"));

    let live_button = gtk::Button::new();
    live_button.add_css_class("live-button");
    live_button.set_valign(gtk::Align::Center);
    live_button.set_tooltip_text(Some("Back to live (Home)"));
    let live_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let live_dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    live_dot.add_css_class("live-dot");
    live_dot.set_valign(gtk::Align::Center);
    let live_label = gtk::Label::new(Some("Live"));
    live_row.append(&live_dot);
    live_row.append(&live_label);
    live_button.set_child(Some(&live_row));

    for (b, name) in [
        (&prev_button, "tl-prev"),
        (&back_button, "tl-back"),
        (&pause_button, "tl-pause"),
        (&forward_button, "tl-forward"),
        (&next_button, "tl-next"),
    ] {
        b.set_widget_name(name);
        controls.append(b);
    }
    speed_dropdown.set_widget_name("tl-speed");
    controls.append(&speed_dropdown);
    live_button.set_widget_name("tl-live");
    controls.append(&live_button);
    root.append(&controls);

    // ---- lanes ----
    let area = gtk::DrawingArea::new();
    area.add_css_class("ruler");
    area.set_widget_name("tl-lanes");
    area.set_hexpand(true);
    area.set_vexpand(false);
    area.set_margin_start(12);
    area.set_margin_end(12);
    area.set_margin_bottom(8);
    area.set_focusable(true);
    root.append(&area);

    let filter = Filter::from_config(&store.config().session);
    for (kind, check) in &filter_checks {
        check.set_active(filter.shows(*kind));
    }
    recordings_check.set_active(filter.recordings);

    let now = Timeline::now_secs();
    let tl = Rc::new(Timeline {
        store: store.clone(),
        services: services.clone(),
        grid: grid.clone(),
        root: root.clone(),
        readout,
        date_label,
        zoom_label,
        tracking_box,
        tracking_cam,
        pause_button,
        pause_icon,
        speed_dropdown,
        live_button,
        live_label,
        filter_checks,
        recordings_check,
        calendar,
        hour_spin,
        minute_spin,
        datetime_popover,
        area,
        zoom: Cell::new(DEFAULT_ZOOM),
        follow: Cell::new(true),
        view_start: Cell::new(Window::ending_now(now, ZOOM_LEVELS[DEFAULT_ZOOM]).start),
        hover: Cell::new(None),
        drag: RefCell::new(None),
        geometry: Cell::new(Geometry::default()),
        filter: Cell::new(filter),
        cache: RefCell::new(Cache::default()),
        fetching: Cell::new(false),
        fetch_generation: Cell::new(0),
        last_position_ms: Cell::new(None),
        shown_state: Cell::new(None),
        keys_installed: Cell::new(false),
        me: std::cell::OnceCell::new(),
    });
    let _ = tl.me.set(Rc::downgrade(&tl));
    DRIVER.with(|d| *d.borrow_mut() = Some(Rc::downgrade(&tl)));

    // ---- wiring: buttons ----
    macro_rules! on_click {
        ($button:expr, |$t:ident| $body:expr) => {{
            let w = Rc::downgrade(&tl);
            $button.connect_clicked(move |_| {
                if let Some($t) = w.upgrade() {
                    $body;
                }
            });
        }};
    }
    on_click!(zoom_out_button, |t| t.zoom_by(1));
    on_click!(zoom_in_button, |t| t.zoom_by(-1));
    on_click!(prev_button, |t| t.jump_event(-1));
    on_click!(next_button, |t| t.jump_event(1));
    on_click!(back_button, |t| t.step(-10));
    on_click!(forward_button, |t| t.step(10));
    on_click!(prev_day_button, |t| t.shift_days(-1));
    on_click!(next_day_button, |t| t.shift_days(1));
    on_click!(download_button, |t| t.download());
    on_click!(go_button, |t| t.go_to_picked());
    on_click!(today_button, |t| t.pick_now());
    on_click!(tl.pause_button, |t| t.toggle_pause());
    on_click!(tl.live_button, |t| t.go_live());
    {
        let w = Rc::downgrade(&tl);
        tl.speed_dropdown.connect_selected_notify(move |dd| {
            let i = dd.selected();
            if let (Some(t), true) = (w.upgrade(), i != gtk::INVALID_LIST_POSITION) {
                t.grid.set_speed_index(i as usize);
            }
        });
    }
    for (kind, check) in &tl.filter_checks {
        let w = Rc::downgrade(&tl);
        let kind = *kind;
        check.connect_toggled(move |c| {
            if let Some(t) = w.upgrade() {
                let mut f = t.filter.get();
                f.set(kind, c.is_active());
                t.set_filter(f);
            }
        });
    }
    {
        let w = Rc::downgrade(&tl);
        tl.recordings_check.connect_toggled(move |c| {
            if let Some(t) = w.upgrade() {
                let mut f = t.filter.get();
                f.recordings = c.is_active();
                t.set_filter(f);
            }
        });
    }
    {
        // opening the date picker preloads the current position
        let w = Rc::downgrade(&tl);
        tl.datetime_popover.connect_show(move |_| {
            if let Some(t) = w.upgrade() {
                let pb = t.grid.playback();
                t.set_picker(t.position_ms(&pb) / 1000);
            }
        });
    }

    // ---- wiring: lanes ----
    {
        let w = Rc::downgrade(&tl);
        tl.area.set_draw_func(move |_, cr, width, height| {
            if let Some(t) = w.upgrade() {
                t.draw(cr, width as f64, height as f64);
            }
        });
    }
    {
        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        let w = Rc::downgrade(&tl);
        drag.connect_drag_begin(move |_, x, y| {
            if let Some(t) = w.upgrade() {
                t.drag_begin(x, y);
            }
        });
        let w = Rc::downgrade(&tl);
        drag.connect_drag_update(move |g, dx, _| {
            if let (Some(t), Some((x0, _))) = (w.upgrade(), g.start_point()) {
                t.drag_update(x0, dx);
            }
        });
        let w = Rc::downgrade(&tl);
        drag.connect_drag_end(move |g, dx, dy| {
            if let (Some(t), Some((x0, y0))) = (w.upgrade(), g.start_point()) {
                t.drag_end(x0, y0, dx, dy);
            }
        });
        tl.area.add_controller(drag);
    }
    {
        let motion = gtk::EventControllerMotion::new();
        let w = Rc::downgrade(&tl);
        motion.connect_motion(move |_, x, y| {
            if let Some(t) = w.upgrade() {
                t.hover.set(Some((x, y)));
                t.area.queue_draw();
            }
        });
        let w = Rc::downgrade(&tl);
        motion.connect_leave(move |_| {
            if let Some(t) = w.upgrade() {
                t.hover.set(None);
                t.area.queue_draw();
            }
        });
        tl.area.add_controller(motion);
    }
    {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let w = Rc::downgrade(&tl);
        scroll.connect_scroll(move |c, dx, dy| {
            let Some(t) = w.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let state = c.current_event_state();
            let delta = if dx.abs() > dy.abs() { dx } else { dy };
            if state.contains(gdk::ModifierType::CONTROL_MASK) {
                let frac = t.hover.get().map(|(x, _)| t.geometry.get().frac_at(x));
                t.zoom_by_at(if delta > 0.0 { 1 } else { -1 }, frac);
            } else {
                // plain or Shift + wheel: pan by a tenth of the window
                let secs = (t.window().secs as f64 * 0.1 * delta.signum()) as i64;
                t.pan_by(secs);
            }
            glib::Propagation::Stop
        });
        tl.area.add_controller(scroll);
    }
    {
        // keyboard shortcuts on the window, while the Live page is up
        let w = Rc::downgrade(&tl);
        tl.area.connect_realize(move |area| {
            let Some(t) = w.upgrade() else { return };
            if t.keys_installed.replace(true) {
                return;
            }
            let Some(window) = area.root().and_then(|r| r.downcast::<gtk::Window>().ok()) else {
                return;
            };
            let keys = gtk::EventControllerKey::new();
            let w = Rc::downgrade(&t);
            keys.connect_key_pressed(move |c, key, _, state| {
                let Some(t) = w.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if t.key(c, key, state) {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            window.add_controller(keys);
        });
    }

    // grid state → controls
    {
        let w = Rc::downgrade(&tl);
        grid.subscribe(move || {
            if let Some(t) = w.upgrade() {
                t.refresh_controls();
                t.area.queue_draw();
                t.maybe_fetch();
            }
        });
    }
    // clock + redraw. This closure holds the only strong reference to the
    // timeline: the Live page lives as long as the window, and every other
    // closure (gestures, draw func, grid listener) only holds a `Weak`.
    {
        let t = tl.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            // slots switch to History asynchronously (after the recording
            // lookup); refresh the controls when the mode actually changes
            let pb = t.grid.playback();
            if t.shown_state.get() != Some((pb.mode, pb.paused)) {
                t.refresh_controls();
            }
            t.refresh_readout();
            t.keep_playhead_visible();
            t.area.queue_draw();
            t.maybe_fetch();
            glib::ControlFlow::Continue
        });
    }
    // `RequestSeek` from other pages (Events "view in timeline")
    {
        let w = Rc::downgrade(&tl);
        store.subscribe(move |change| {
            if let (Change::RequestSeek(secs, camera), Some(t)) = (change, w.upgrade()) {
                t.store
                    .update(Change::Page, |s| s.page = crate::state::Page::Live);
                // bind first: the `Ref` must be gone before `update` borrows mutably
                let slot = t.store.get().slot_of(*camera);
                if let Some(i) = slot {
                    t.store
                        .update(Change::SelectedSlot, |s| s.selected_slot = Some(i));
                }
                t.seek_to(*secs);
            }
        });
    }

    tl.refresh_controls();
    tl.refresh_readout();
    tl.maybe_fetch();
    root.upcast()
}

impl Timeline {
    fn now_secs() -> i64 {
        Local::now().timestamp()
    }

    /// The visible window: glued to now while following live.
    fn window(&self) -> Window {
        let secs = ZOOM_LEVELS[self.zoom.get()];
        if self.follow.get() {
            Window::ending_now(Self::now_secs(), secs)
        } else {
            Window {
                start: self.view_start.get(),
                secs,
            }
        }
    }

    /// Stop following live and show `window`.
    fn set_window(&self, window: Window) {
        let now = Self::now_secs();
        let start = window.start.clamp(
            now - MAX_HISTORY_SECS,
            now - (window.secs as f64 * 0.5) as i64,
        );
        self.follow.set(false);
        self.view_start.set(start);
        self.area.queue_draw();
    }

    /// Lanes: cameras in slot order (deduplicated), with names.
    fn lanes(&self) -> Vec<(i64, String)> {
        let s = self.store.get();
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for id in self.grid.camera_ids() {
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            let name = s
                .camera(id)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| format!("Camera {id}"));
            out.push((id, name));
        }
        out
    }

    fn camera_name(&self, id: i64) -> String {
        self.store
            .get()
            .camera(id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| format!("Camera {id}"))
    }

    /// Position shown: live → now; history → the tracked slot's position.
    fn position_ms(&self, pb: &Playback) -> i64 {
        match pb.mode {
            Mode::Live => Local::now().timestamp_millis(),
            Mode::History => {
                if let Some(ms) = pb.position_ms {
                    self.last_position_ms.set(Some(ms));
                    ms
                } else {
                    self.last_position_ms
                        .get()
                        .unwrap_or_else(|| Local::now().timestamp_millis())
                }
            }
        }
    }

    fn refresh_readout(&self) {
        let pb = self.grid.playback();
        let ms = match &*self.drag.borrow() {
            Some(Drag::Scrub { time }) => *time * 1000,
            _ => self.position_ms(&pb),
        };
        let t = local_from_ms(ms);
        self.readout.set_text(&format_readout(&t));
        self.date_label.set_text(&format_date(&t));
        let history = pb.mode == Mode::History;
        set_class(&self.readout, "history", history);
        set_class(&self.readout, "live", !history);
    }

    fn refresh_controls(&self) {
        let pb = self.grid.playback();
        self.shown_state.set(Some((pb.mode, pb.paused)));
        let at_edge = pb.mode == Mode::Live;
        if !at_edge {
            self.last_position_ms
                .set(pb.position_ms.or(self.last_position_ms.get()));
        } else {
            self.last_position_ms.set(None);
        }

        match self.grid.tracked_camera() {
            Some(cam) => {
                self.tracking_cam.set_text(&cam.name);
                self.tracking_box.set_visible(true);
            }
            None => self.tracking_box.set_visible(false),
        }

        set_class(&self.pause_button, "on", pb.paused);
        self.pause_icon
            .set_icon_name(Some(&icons::name(if pb.paused {
                "play_arrow"
            } else {
                "pause"
            })));
        self.pause_button.set_tooltip_text(Some(if pb.paused {
            "Play (Space)"
        } else {
            "Pause (Space)"
        }));

        self.speed_dropdown.set_sensitive(!at_edge);
        if self.speed_dropdown.selected() != pb.speed_index as u32 {
            self.speed_dropdown.set_selected(pb.speed_index as u32);
        }

        let live = at_edge && self.follow.get();
        set_class(&self.live_button, "live-edge", live);
        set_class(&self.live_button, "primary", !live);
        self.live_label
            .set_text(if live { "Live" } else { "Go live" });
        self.zoom_label
            .set_text(&format_zoom(ZOOM_LEVELS[self.zoom.get()]));

        // strip height: ruler + one lane per camera (+ one placeholder lane)
        let n = self.lanes().len().max(1);
        let body = (RULER_H + n as f64 * lane_height(n) + PAD_BOTTOM).ceil() as i32;
        let body = body.max(MIN_HEIGHT - CONTROLS_H - 8);
        if self.area.height_request() != body {
            self.area.set_size_request(-1, body);
        }
        self.refresh_readout();
    }

    /// History: when the playhead leaves a non-following window, shift it.
    fn keep_playhead_visible(&self) {
        if self.follow.get() || self.drag.borrow().is_some() {
            return;
        }
        let pb = self.grid.playback();
        if pb.mode != Mode::History {
            return;
        }
        let pos = self.position_ms(&pb) / 1000;
        let w = self.window();
        let f = w.frac(pos);
        if !(0.0..=0.98).contains(&f) {
            self.set_window(Window {
                start: pos - (w.secs as f64 * 0.25) as i64,
                secs: w.secs,
            });
        }
    }

    // --- navigation --------------------------------------------------------------

    fn zoom_by(&self, delta: i32) {
        // anchor on the playhead when it is in view, else the centre
        let pb = self.grid.playback();
        let pos = self.position_ms(&pb) / 1000;
        let w = self.window();
        let f = w.frac(pos);
        let anchor = (0.0..=1.0).contains(&f).then_some(f);
        self.zoom_by_at(delta, anchor);
    }

    /// Zoom in (−1) or out (+1), keeping the time at `anchor_frac` fixed
    /// (follow-live mode keeps the live edge instead).
    fn zoom_by_at(&self, delta: i32, anchor_frac: Option<f64>) {
        let z = (self.zoom.get() as i32 + delta).clamp(0, ZOOM_LEVELS.len() as i32 - 1) as usize;
        if z == self.zoom.get() {
            return;
        }
        let old = self.window();
        self.zoom.set(z);
        if !self.follow.get() {
            let frac = anchor_frac.unwrap_or(0.5).clamp(0.0, 1.0);
            let anchor_t = old.time_at(frac);
            self.set_window(old.zoomed(ZOOM_LEVELS[z], anchor_t, frac));
        }
        self.refresh_controls();
        self.area.queue_draw();
        self.maybe_fetch();
    }

    fn pan_by(&self, secs: i64) {
        let w = self.window();
        self.set_window(Window {
            start: w.start + secs,
            secs: w.secs,
        });
        self.maybe_fetch();
    }

    /// Seek every slot to `t` (history), or back to live at the live edge.
    fn seek_to(&self, t: i64) {
        let now = Self::now_secs();
        if is_live_edge(t, now, self.window().secs) {
            self.go_live();
            return;
        }
        let t = t.min(now);
        let w = self.window();
        if !(0.0..=1.0).contains(&w.frac(t)) {
            self.set_window(Window {
                start: t - (w.secs as f64 * 0.3) as i64,
                secs: w.secs,
            });
        }
        self.last_position_ms.set(Some(t * 1000));
        self.grid.seek(t);
        self.refresh_controls();
    }

    fn go_live(&self) {
        self.follow.set(true);
        if self.grid.playback().mode == Mode::History {
            self.grid.go_live();
        }
        self.refresh_controls();
        self.area.queue_draw();
    }

    fn toggle_pause(&self) {
        let paused = self.grid.playback().paused;
        self.grid.set_paused(!paused);
    }

    /// Back/Forward by `delta_secs` from the current position.
    fn step(&self, delta_secs: i64) {
        let pb = self.grid.playback();
        let pos = self.position_ms(&pb) / 1000 + delta_secs;
        self.seek_to(pos);
    }

    /// ±1 day around the current position.
    fn shift_days(&self, days: i64) {
        let pb = self.grid.playback();
        let pos = self.position_ms(&pb) / 1000 + days * 86400;
        let w = self.window();
        self.set_window(Window {
            start: w.start + days * 86400,
            secs: w.secs,
        });
        self.seek_to(pos);
        self.maybe_fetch();
    }

    /// Previous/Next event start, among the tracked camera's events when a
    /// slot is selected, else any camera's; only kinds the filter shows.
    fn jump_event(&self, direction: i32) {
        let pb = self.grid.playback();
        let pos = self.position_ms(&pb) / 1000;
        let camera = self
            .store
            .get()
            .selected_slot
            .and_then(|_| self.grid.tracked_camera().map(|c| c.id));
        let filter = self.filter.get();
        let target = {
            let cache = self.cache.borrow();
            let (from, to) = cache.span().unwrap_or((pos, pos));
            let starts: Vec<i64> = cache
                .events(from, to)
                .iter()
                .filter(|e| camera.is_none_or(|c| e.camera_id == c))
                .filter(|e| filter.shows(e.kind))
                .map(|e| e.start)
                .collect();
            if direction < 0 {
                prev_start(&starts, pos)
            } else {
                next_start(&starts, pos)
            }
        };
        match target {
            Some(t) => self.seek_to(t),
            None => self.store.toast(if direction < 0 {
                "No earlier event loaded · pan back to load more"
            } else {
                "No later event loaded"
            }),
        }
    }

    fn set_filter(&self, f: Filter) {
        if self.filter.replace(f) == f {
            return;
        }
        for (kind, check) in &self.filter_checks {
            if check.is_active() != f.shows(*kind) {
                check.set_active(f.shows(*kind));
            }
        }
        if self.recordings_check.is_active() != f.recordings {
            self.recordings_check.set_active(f.recordings);
        }
        f.write_config(&mut self.store.config().session);
        self.store.save_soon();
        self.area.queue_draw();
    }

    // --- date picker ---------------------------------------------------------------

    fn set_picker(&self, secs: i64) {
        let t = local_from_secs(secs);
        if let Ok(d) = glib::DateTime::from_local(
            t.date().format("%Y").to_string().parse().unwrap_or(1970),
            t.date().format("%m").to_string().parse().unwrap_or(1),
            t.date().format("%d").to_string().parse().unwrap_or(1),
            0,
            0,
            0.0,
        ) {
            self.calendar.select_day(&d);
        }
        self.hour_spin
            .set_value(t.format("%H").to_string().parse().unwrap_or(0.0));
        self.minute_spin
            .set_value(t.format("%M").to_string().parse().unwrap_or(0.0));
    }

    fn pick_now(&self) {
        self.set_picker(Self::now_secs());
    }

    /// The picker's local date + time as unix seconds.
    fn picked_secs(&self) -> Option<i64> {
        let d = self.calendar.date();
        let naive =
            chrono::NaiveDate::from_ymd_opt(d.year(), d.month() as u32, d.day_of_month() as u32)?
                .and_hms_opt(
                self.hour_spin.value_as_int().clamp(0, 23) as u32,
                self.minute_spin.value_as_int().clamp(0, 59) as u32,
                0,
            )?;
        Local
            .from_local_datetime(&naive)
            .earliest()
            .map(|t| t.timestamp())
    }

    fn go_to_picked(&self) {
        let Some(t) = self.picked_secs() else {
            self.store.toast("Invalid date");
            return;
        };
        self.datetime_popover.popdown();
        let now = Self::now_secs();
        if t >= now - 60 {
            self.go_live();
            return;
        }
        let w = self.window();
        self.set_window(Window {
            start: t - (w.secs as f64 * 0.3) as i64,
            secs: w.secs,
        });
        self.seek_to(t);
        self.maybe_fetch();
    }

    // --- download -------------------------------------------------------------------

    /// Download the tracked camera's recording under the playhead.
    fn download(&self) {
        let Some(cam) = self.grid.tracked_camera() else {
            self.store.toast("No camera to download from");
            return;
        };
        let Some(client) = self.services.client() else {
            self.store.toast("Not connected");
            return;
        };
        let pb = self.grid.playback();
        let pos = self.position_ms(&pb) / 1000;
        let dir = crate::pages::browse::download_dir(&self.store);
        let name = cam.name.clone();
        let fut = self.services.run(async move {
            let filter = ss_api::RecordingFilter::camera(cam.id)
                .between(pos - 12 * 3600, pos + 3600)
                .page(0, 500);
            let page = client
                .list_recordings(&filter)
                .await
                .map_err(|e| e.to_string())?;
            let rec = page
                .items
                .into_iter()
                .find(|r| r.start_time <= pos && pos <= r.stop_time)
                .ok_or_else(|| "no recording at that time".to_string())?;
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| e.to_string())?;
            let file = dir.join(crate::pages::browse::media_filename(
                &cam.name,
                rec.start_time,
                "mp4",
            ));
            let mut out = tokio::fs::File::create(&file)
                .await
                .map_err(|e| e.to_string())?;
            let bytes = client
                .download_recording(rec.id, &mut out)
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((file, bytes))
        });
        self.store.toast(format!("Downloading · {name}"));
        let store = self.store.clone();
        glib::spawn_future_local(async move {
            match fut.await {
                Ok((file, bytes)) => {
                    tracing::info!("downloaded {bytes} bytes to {}", file.display());
                    store.toast(format!(
                        "Saved {} · {}",
                        file.file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        format_bytes(bytes)
                    ));
                }
                Err(e) => {
                    tracing::warn!("download: {e}");
                    store.toast(format!("Download failed · {e}"));
                }
            }
        });
    }

    // --- gestures ---------------------------------------------------------------------

    fn drag_begin(&self, x: f64, y: f64) {
        let g = self.geometry.get();
        let w = self.window();
        let pb = self.grid.playback();
        let pos = self.position_ms(&pb) as f64 / 1000.0;
        let play_x = g.axis_x(w.frac_f(pos));
        if x >= g.name_w && (x - play_x).abs() <= SCRUB_GRAB_PX && y >= 0.0 {
            *self.drag.borrow_mut() = Some(Drag::Scrub {
                time: w.time_at(g.frac_at(x)),
            });
        } else {
            *self.drag.borrow_mut() = Some(Drag::Pan {
                start_view: w.start,
            });
        }
    }

    fn drag_update(&self, x0: f64, dx: f64) {
        if dx.abs() < CLICK_SLOP_PX && matches!(&*self.drag.borrow(), Some(Drag::Pan { .. })) {
            return;
        }
        let g = self.geometry.get();
        let w = self.window();
        let x = x0 + dx;
        let mut drag = self.drag.borrow_mut();
        match &mut *drag {
            Some(Drag::Pan { start_view }) => {
                let secs = -(dx / g.axis_w()) * w.secs as f64;
                let start = *start_view + secs as i64;
                drop(drag);
                self.set_window(Window {
                    start,
                    secs: w.secs,
                });
            }
            Some(Drag::Scrub { time }) => {
                *time = w
                    .time_at(g.frac_at(x).clamp(0.0, 1.0))
                    .min(Self::now_secs());
                drop(drag);
                self.refresh_readout();
                self.area.queue_draw();
            }
            None => {}
        }
    }

    fn drag_end(&self, x0: f64, y0: f64, dx: f64, dy: f64) {
        let drag = self.drag.borrow_mut().take();
        match drag {
            Some(Drag::Scrub { time }) => {
                if dx.abs() < CLICK_SLOP_PX && dy.abs() < CLICK_SLOP_PX {
                    self.click_at(x0, y0);
                } else {
                    self.seek_to(time);
                }
            }
            Some(Drag::Pan { .. }) => {
                if dx.abs() < CLICK_SLOP_PX && dy.abs() < CLICK_SLOP_PX {
                    self.click_at(x0, y0);
                } else {
                    self.maybe_fetch();
                }
            }
            None => {}
        }
        self.area.queue_draw();
    }

    /// Click: name column selects the camera's slot; ruler seeks every
    /// slot; a lane selects its camera and seeks (to an event's start when
    /// one is under the pointer).
    fn click_at(&self, x: f64, y: f64) {
        let g = self.geometry.get();
        let w = self.window();
        let lanes = self.lanes();
        let lane = g.lane_at(y);
        if let Some(i) = lane
            && let Some((camera_id, _)) = lanes.get(i)
        {
            let slot = self.store.get().slot_of(*camera_id);
            if let Some(slot) = slot {
                self.store
                    .update(Change::SelectedSlot, |s| s.selected_slot = Some(slot));
            }
            if x < g.name_w {
                return;
            }
            let t = w.time_at(g.frac_at(x));
            match self.event_at(x, i, &lanes) {
                Some(ev) => self.seek_to(ev.start),
                None => self.seek_to(t),
            }
            return;
        }
        if x >= g.name_w {
            self.seek_to(w.time_at(g.frac_at(x)));
        }
    }

    /// The (visible) event marker under pixel `x` in `lane`.
    fn event_at(&self, x: f64, lane: usize, lanes: &[(i64, String)]) -> Option<EventMark> {
        let g = self.geometry.get();
        let w = self.window();
        let camera_id = lanes.get(lane)?.0;
        let filter = self.filter.get();
        let cache = self.cache.borrow();
        let mut best: Option<(f64, EventMark)> = None;
        for ev in cache.events(w.start - 86400, w.end()) {
            if ev.camera_id != camera_id || !filter.shows(ev.kind) {
                continue;
            }
            let Some((fa, fb)) = clip_span((ev.start, ev.end), &w) else {
                continue;
            };
            let x0 = g.axis_x(fa) - 2.0;
            let x1 = (g.axis_x(fb) + 2.0).max(x0 + 5.0);
            if x >= x0 && x <= x1 {
                let d = (x - (x0 + x1) / 2.0).abs();
                if best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                    best = Some((d, ev));
                }
            }
        }
        best.map(|(_, e)| e)
    }

    /// Keyboard: Space pause, ←/→ ±10 s (Shift: ±1 min), Home = live,
    /// Escape = back from a focused slot to the layout.
    /// Only while the Live page is up and no text field has focus.
    fn key(&self, c: &gtk::EventControllerKey, key: gdk::Key, state: gdk::ModifierType) -> bool {
        if self.store.get().page != crate::state::Page::Live {
            return false;
        }
        if let Some(focus) = c
            .widget()
            .and_then(|w| w.root())
            .and_then(|r| r.downcast::<gtk::Window>().ok())
            .and_then(|w| GtkWindowExt::focus(&w))
            && (focus.is::<gtk::Text>()
                || focus.is::<gtk::Editable>()
                || focus.is::<gtk::SpinButton>())
        {
            return false;
        }
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let step = if shift { 60 } else { 10 };
        match key {
            gdk::Key::space => self.toggle_pause(),
            gdk::Key::Left => self.step(-step),
            gdk::Key::Right => self.step(step),
            gdk::Key::Home => self.go_live(),
            gdk::Key::Escape => {
                if self.store.get().focused_slot.is_none() {
                    return false;
                }
                self.store
                    .update(crate::state::Change::FocusedSlot, |s| s.focused_slot = None);
            }
            _ => return false,
        }
        true
    }

    // --- data -----------------------------------------------------------------------

    fn maybe_fetch(&self) {
        if self.fetching.get() {
            return;
        }
        let Some(client) = self.services.client() else {
            return;
        };
        let mut ids: Vec<i64> = self.lanes().into_iter().map(|(id, _)| id).collect();
        ids.sort_unstable();
        if ids.is_empty() {
            return;
        }
        let window = self.window();
        let now = Self::now_secs();
        let margin = window.secs / 4;
        let (from, to) = {
            let mut cache = self.cache.borrow_mut();
            cache.reset_for(&ids);
            let missing = cache.missing(window.start - margin, window.end() + margin, now);
            let Some(run) = first_run(&missing, MAX_CHUNKS_PER_FETCH) else {
                return;
            };
            run
        };
        self.fetching.set(true);
        let generation = self.fetch_generation.get() + 1;
        self.fetch_generation.set(generation);
        tracing::debug!(
            "timeline: fetching {} cameras {from}..{to} ({} h)",
            ids.len(),
            (to - from) / 3600
        );
        let fut = self
            .services
            .run(fetch_range(client, ids.clone(), from, to));
        let Some(w) = self.me.get().cloned() else {
            self.fetching.set(false);
            return;
        };
        glib::spawn_future_local(async move {
            let fetched = fut.await;
            let Some(t) = w.upgrade() else { return };
            t.fetching.set(false);
            if t.fetch_generation.get() != generation {
                return;
            }
            {
                let mut cache = t.cache.borrow_mut();
                if cache.ids != ids {
                    return;
                }
                cache.store(from, to, &fetched, Self::now_secs());
            }
            tracing::debug!(
                "timeline: {} recordings spans, {} events for {from}..{to}",
                fetched.recordings.values().map(Vec::len).sum::<usize>(),
                fetched.events.len()
            );
            t.area.queue_draw();
        });
    }

    // --- drawing -------------------------------------------------------------------

    fn draw(&self, cr: &cairo::Context, width: f64, height: f64) {
        let palette = Palette::for_theme(self.store.get().theme);
        let window = self.window();
        let now = Self::now_secs();
        let pb = self.grid.playback();
        let history = pb.mode == Mode::History;
        let pos_ms = self.position_ms(&pb);
        let lanes = self.lanes();
        let n = lanes.len().max(1);
        let lane_h = lane_height(n).min(((height - RULER_H - PAD_BOTTOM) / n as f64).max(8.0));
        let g = Geometry {
            width,
            height,
            name_w: NAME_W,
            ruler_h: RULER_H,
            lane_h,
            lanes: lanes.len(),
        };
        self.geometry.set(g);
        let filter = self.filter.get();
        let scrub = match &*self.drag.borrow() {
            Some(Drag::Scrub { time }) => Some(*time),
            _ => None,
        };

        // frame: bg + 1 px border, radius 4, clip to it
        rounded_rect(cr, 0.5, 0.5, width - 1.0, height - 1.0, 4.0);
        palette.set(cr, palette.bg);
        let _ = cr.fill_preserve();
        palette.set(cr, palette.border);
        cr.set_line_width(1.0);
        let _ = cr.stroke_preserve();
        cr.clip();

        // name column
        palette.set(cr, palette.surface);
        cr.rectangle(0.0, 0.0, g.name_w, height);
        let _ = cr.fill();
        palette.set(cr, palette.border);
        cr.rectangle(g.name_w - 0.5, 0.0, 1.0, height);
        let _ = cr.fill();

        // lane backgrounds (alternating) + separators
        for i in 0..n {
            let y = g.lane_y(i);
            if i % 2 == 1 {
                cr.set_source_rgba(
                    palette.surface.0,
                    palette.surface.1,
                    palette.surface.2,
                    0.35,
                );
                cr.rectangle(g.name_w, y, width - g.name_w, lane_h);
                let _ = cr.fill();
            }
            palette.set(cr, palette.border);
            cr.rectangle(0.0, (y + lane_h).round() - 0.5, width, 1.0);
            let _ = cr.fill();
        }
        // ruler base line
        palette.set(cr, palette.border);
        cr.rectangle(g.name_w, RULER_H.round() - 0.5, width - g.name_w, 1.0);
        let _ = cr.fill();

        cr.select_font_face(
            "IBM Plex Mono",
            cairo::FontSlant::Normal,
            cairo::FontWeight::Normal,
        );

        // ticks + labels
        cr.set_font_size(10.0);
        let lanes_bottom = g.lane_y(n);
        for tick in window.ticks() {
            let x = g.axis_x(window.frac(tick)).round() + 0.5;
            cr.set_source_rgba(palette.border.0, palette.border.1, palette.border.2, 0.9);
            cr.move_to(x, RULER_H - 6.0);
            cr.line_to(x, lanes_bottom);
            let _ = cr.stroke();
            palette.set(cr, palette.muted);
            let text = format_tick(&local_from_secs(tick));
            let tw = cr
                .text_extents(&text)
                .map(|e| e.x_advance())
                .unwrap_or(30.0);
            if x + 3.0 + tw < width - 2.0 {
                cr.move_to(x + 3.0, 13.0);
                let _ = cr.show_text(&text);
            }
        }

        // lanes: names, recordings, events
        let tracked = self.grid.tracked_camera().map(|c| c.id);
        {
            let cache = self.cache.borrow();
            let events = cache.events(window.start - 86400, window.end());
            cr.set_font_size(11.0);
            for (i, (camera_id, name)) in lanes.iter().enumerate() {
                let y = g.lane_y(i);
                // name
                cr.select_font_face(
                    "IBM Plex Sans",
                    cairo::FontSlant::Normal,
                    if Some(*camera_id) == tracked {
                        cairo::FontWeight::Bold
                    } else {
                        cairo::FontWeight::Normal
                    },
                );
                if Some(*camera_id) == tracked {
                    palette.set(cr, palette.blue);
                    cr.rectangle(0.0, y + 2.0, 3.0, lane_h - 4.0);
                    let _ = cr.fill();
                    palette.set(cr, palette.text);
                } else {
                    palette.set(cr, palette.muted);
                }
                let shown = fit_text(cr, name, g.name_w - 14.0);
                cr.move_to(8.0, y + lane_h / 2.0 + 4.0);
                let _ = cr.show_text(&shown);
                // recordings
                if filter.recordings {
                    let spans = cache.recordings(*camera_id, window.start, window.end());
                    cr.set_source_rgba(palette.blue.0, palette.blue.1, palette.blue.2, 0.45);
                    let bar_h = (lane_h - 8.0).max(3.0);
                    draw_spans(cr, &window, &g, &spans, y + (lane_h - bar_h) / 2.0, bar_h);
                }
                // events on top
                let mark_h = (lane_h - 4.0).max(4.0);
                for kind in Kind::ALL {
                    if !filter.shows(kind) {
                        continue;
                    }
                    let spans: Vec<(i64, i64)> = events
                        .iter()
                        .filter(|e| e.camera_id == *camera_id && e.kind == kind)
                        .map(|e| (e.start, e.end))
                        .collect();
                    if spans.is_empty() {
                        continue;
                    }
                    palette.set(cr, palette.kind(kind));
                    draw_spans(cr, &window, &g, &spans, y + (lane_h - mark_h) / 2.0, mark_h);
                }
            }
            if lanes.is_empty() {
                cr.select_font_face(
                    "IBM Plex Sans",
                    cairo::FontSlant::Normal,
                    cairo::FontWeight::Normal,
                );
                palette.set(cr, palette.muted);
                cr.move_to(g.name_w + 10.0, g.lane_y(0) + lane_h / 2.0 + 4.0);
                let _ = cr.show_text("No cameras in this layout");
            }
        }

        // live edge: dashed line at now (when in view)
        let now_f = window.frac(now);
        if (0.0..=1.0).contains(&now_f) {
            let x = g.axis_x(now_f).round() + 0.5;
            palette.set(cr, palette.muted);
            cr.set_dash(&[3.0, 3.0], 0.0);
            cr.move_to(x, 0.0);
            cr.line_to(x, height);
            let _ = cr.stroke();
            cr.set_dash(&[], 0.0);
            // future is hatched out
            cr.set_source_rgba(palette.bg.0, palette.bg.1, palette.bg.2, 0.55);
            cr.rectangle(x, RULER_H, width - x, height - RULER_H);
            let _ = cr.fill();
        }

        // hover: line + bubble
        if let (Some((hx, hy)), None) = (self.hover.get(), scrub) {
            if hx >= g.name_w {
                let t = window.time_at(g.frac_at(hx));
                let text = match g.lane_at(hy) {
                    Some(i) => match self.event_at(hx, i, &lanes) {
                        Some(ev) => format!(
                            "{} · {} · {} ({})",
                            ev.kind.label(),
                            self.camera_name(ev.camera_id),
                            format_readout(&local_from_secs(ev.start)),
                            format_duration(ev.end - ev.start)
                        ),
                        None => format!(
                            "{} · {}",
                            lanes.get(i).map(|l| l.1.as_str()).unwrap_or(""),
                            format_readout(&local_from_secs(t))
                        ),
                    },
                    None => format_readout(&local_from_secs(t)),
                };
                let x = hx.round() + 0.5;
                cr.set_source_rgba(palette.text.0, palette.text.1, palette.text.2, 0.35);
                cr.move_to(x, RULER_H);
                cr.line_to(x, height);
                let _ = cr.stroke();
                draw_bubble(
                    cr,
                    &palette,
                    &text,
                    hx,
                    height - PAD_BOTTOM - 18.0,
                    palette.surface,
                    palette.text,
                    width,
                );
            }
        }

        // playhead
        let play_t = match scrub {
            Some(t) => t as f64,
            None if history => pos_ms as f64 / 1000.0,
            None => now as f64,
        };
        let play_f = window.frac_f(play_t);
        if (-0.01..=1.01).contains(&play_f) {
            let x = g.axis_x(play_f).round();
            let colour = if history || scrub.is_some() {
                palette.blue
            } else {
                palette.red
            };
            palette.set(cr, colour);
            cr.rectangle(x - 1.0, 0.0, 2.0, height);
            let _ = cr.fill();
            // head triangle in the ruler
            cr.move_to(x - 5.0, 0.0);
            cr.line_to(x + 5.0, 0.0);
            cr.line_to(x, 6.0);
            cr.close_path();
            let _ = cr.fill();
            if history || scrub.is_some() {
                let text = format_readout(&local_from_ms((play_t * 1000.0) as i64));
                draw_bubble(cr, &palette, &text, x, 3.0, colour, (1.0, 1.0, 1.0), width);
            }
        } else if history {
            // off-screen playhead: arrow at the edge
            palette.set(cr, palette.blue);
            let (x, dir) = if play_f < 0.0 {
                (g.name_w + 8.0, -1.0)
            } else {
                (width - 8.0, 1.0)
            };
            cr.move_to(x, RULER_H / 2.0 - 5.0);
            cr.line_to(x + 6.0 * dir, RULER_H / 2.0);
            cr.line_to(x, RULER_H / 2.0 + 5.0);
            cr.close_path();
            let _ = cr.fill();
        }

        // legend bottom-right (only when the lanes leave room)
        if height - lanes_bottom >= 16.0 {
            cr.select_font_face(
                "IBM Plex Mono",
                cairo::FontSlant::Normal,
                cairo::FontWeight::Normal,
            );
            cr.set_font_size(9.0);
            let mut x = width - 8.0;
            let mut items: Vec<(&str, (f64, f64, f64))> = Vec::new();
            if filter.recordings {
                items.push(("recording", palette.blue));
            }
            for kind in Kind::ALL {
                if filter.shows(kind) {
                    items.push((kind.label(), palette.kind(kind)));
                }
            }
            for (text, colour) in items.iter().rev() {
                let ext = cr.text_extents(text).map(|e| e.x_advance()).unwrap_or(40.0);
                x -= ext;
                palette.set(cr, palette.muted);
                cr.move_to(x, height - 5.0);
                let _ = cr.show_text(text);
                x -= 10.0;
                palette.set(cr, *colour);
                cr.rectangle(x, height - 11.0, 7.0, 7.0);
                let _ = cr.fill();
                x -= 10.0;
            }
        }
    }
}

/// Driver entry point (`crates/synoptic/src/driver.rs`): `None` when `cmd`
/// is not a timeline command.
pub fn driver_command(cmd: &str, args: &[&str]) -> Option<Result<String, String>> {
    const COMMANDS: [&str; 9] = [
        "seek",
        "golive",
        "zoom",
        "pan",
        "step",
        "tl-filter",
        "tl-hover",
        "tl-state",
        "tl-date",
    ];
    if !COMMANDS.contains(&cmd) {
        return None;
    }
    let tl = DRIVER.with(|d| d.borrow().as_ref().and_then(Weak::upgrade));
    Some(match tl {
        Some(t) => t.driver(cmd, args),
        None => Err("timeline not built".to_string()),
    })
}

impl Timeline {
    fn driver(&self, cmd: &str, args: &[&str]) -> Result<String, String> {
        let num = |i: usize| -> Result<i64, String> {
            args.get(i)
                .and_then(|s| s.parse::<i64>().ok())
                .ok_or_else(|| format!("{cmd}: expected a number"))
        };
        match cmd {
            "seek" => {
                let v = num(0)?;
                let secs = if v <= 0 { Self::now_secs() + v } else { v };
                self.seek_to(secs);
                Ok(format!("seek {}", format_readout(&local_from_secs(secs))))
            }
            "golive" => {
                self.go_live();
                Ok(String::new())
            }
            "step" => {
                self.step(num(0)?);
                Ok(String::new())
            }
            "zoom" => {
                match args.first().copied() {
                    Some("in") => self.zoom_by(-1),
                    Some("out") => self.zoom_by(1),
                    Some(s) => {
                        let secs: i64 = s.parse().map_err(|_| "zoom <in|out|secs>")?;
                        let idx = ZOOM_LEVELS
                            .iter()
                            .position(|&z| z == secs)
                            .ok_or_else(|| format!("zoom levels: {ZOOM_LEVELS:?}"))?;
                        let delta = idx as i32 - self.zoom.get() as i32;
                        self.zoom_by_at(delta, None);
                    }
                    None => return Err("zoom <in|out|secs>".into()),
                }
                Ok(format_zoom(ZOOM_LEVELS[self.zoom.get()]))
            }
            "pan" => {
                self.pan_by(num(0)?);
                let w = self.window();
                Ok(format!(
                    "window {}..{}",
                    format_readout(&local_from_secs(w.start)),
                    format_readout(&local_from_secs(w.end()))
                ))
            }
            "tl-filter" => {
                let what = args.first().copied().unwrap_or("");
                let on = match args.get(1).copied() {
                    Some("on") => true,
                    Some("off") => false,
                    _ => {
                        return Err(
                            "tl-filter <motion|alarm|manual|other|recordings> <on|off>".into()
                        );
                    }
                };
                let mut f = self.filter.get();
                if what == "recordings" {
                    f.recordings = on;
                } else if let Some(k) = Kind::from_key(what) {
                    f.set(k, on);
                } else {
                    return Err("unknown filter".into());
                }
                self.set_filter(f);
                Ok(format!("{f:?}"))
            }
            "tl-hover" => {
                // tl-hover <frac 0..1> <lane | ruler | off>
                let g = self.geometry.get();
                match args.first().copied() {
                    Some("off") | None => self.hover.set(None),
                    Some(fs) => {
                        let frac: f64 = fs.parse().map_err(|_| "tl-hover <frac> <lane|ruler>")?;
                        let y = match args.get(1).copied() {
                            Some("ruler") | None => g.ruler_h / 2.0,
                            Some(l) => {
                                let lane: usize = l.parse().map_err(|_| "lane index")?;
                                g.lane_y(lane) + g.lane_h / 2.0
                            }
                        };
                        self.hover.set(Some((g.axis_x(frac), y)));
                    }
                }
                self.area.queue_draw();
                Ok(String::new())
            }
            "tl-date" => {
                // tl-date <YYYY-MM-DD> <HH:MM>
                let d = args.first().ok_or("tl-date <YYYY-MM-DD> <HH:MM>")?;
                let hm = args.get(1).copied().unwrap_or("00:00");
                let naive = NaiveDateTime::parse_from_str(&format!("{d} {hm}"), "%Y-%m-%d %H:%M")
                    .map_err(|e| e.to_string())?;
                let secs = Local
                    .from_local_datetime(&naive)
                    .earliest()
                    .ok_or("ambiguous local time")?
                    .timestamp();
                self.set_picker(secs);
                self.go_to_picked();
                Ok(format!("picked {}", self.picked_secs().unwrap_or(0)))
            }
            "tl-state" => {
                let pb = self.grid.playback();
                let w = self.window();
                let cache = self.cache.borrow();
                let lanes = self.lanes();
                let pos = self.position_ms(&pb);
                Ok(format!(
                    "mode={:?} paused={} speed={} pos={} ({}) follow={} zoom={} window={}..{} lanes={} focus={:?} chunks={} events={} filter={:?} height={}",
                    pb.mode,
                    pb.paused,
                    SPEEDS[pb.speed_index].0,
                    pos,
                    format_readout(&local_from_ms(pos)),
                    self.follow.get(),
                    format_zoom(w.secs),
                    format_readout(&local_from_secs(w.start)),
                    format_readout(&local_from_secs(w.end())),
                    lanes.len(),
                    self.store.get().focused_slot,
                    cache.chunk_count(),
                    cache.event_count(),
                    self.filter.get(),
                    self.root.height(),
                ))
            }
            _ => Err("unknown command".into()),
        }
    }
}

fn draw_spans(
    cr: &cairo::Context,
    window: &Window,
    g: &Geometry,
    spans: &[(i64, i64)],
    y: f64,
    h: f64,
) {
    for &(a, b) in spans {
        if let Some((fa, fb)) = clip_span((a, b), window) {
            let x0 = g.axis_x(fa);
            let x1 = (g.axis_x(fb)).max(x0 + 2.0);
            cr.rectangle(x0, y, x1 - x0, h);
        }
    }
    let _ = cr.fill();
}

/// A small rounded label centred on `x` at `y`, kept inside `width`.
#[allow(clippy::too_many_arguments)]
fn draw_bubble(
    cr: &cairo::Context,
    palette: &Palette,
    text: &str,
    x: f64,
    y: f64,
    bg: (f64, f64, f64),
    fg: (f64, f64, f64),
    width: f64,
) {
    cr.select_font_face(
        "IBM Plex Mono",
        cairo::FontSlant::Normal,
        cairo::FontWeight::Bold,
    );
    cr.set_font_size(10.0);
    let tw = cr.text_extents(text).map(|e| e.x_advance()).unwrap_or(50.0);
    let bw = tw + 10.0;
    let bx = (x - bw / 2.0).clamp(2.0, (width - bw - 2.0).max(2.0));
    rounded_rect(cr, bx, y, bw, 16.0, 3.0);
    palette.set(cr, bg);
    let _ = cr.fill_preserve();
    cr.set_source_rgba(palette.border.0, palette.border.1, palette.border.2, 0.8);
    cr.set_line_width(1.0);
    let _ = cr.stroke();
    palette.set(cr, fg);
    cr.move_to(bx + 5.0, y + 12.0);
    let _ = cr.show_text(text);
}

/// Shorten `text` with an ellipsis until it fits `max_w` in the current font.
fn fit_text(cr: &cairo::Context, text: &str, max_w: f64) -> String {
    let w = |s: &str| cr.text_extents(s).map(|e| e.x_advance()).unwrap_or(0.0);
    if w(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut n = chars.len();
    while n > 1 {
        n -= 1;
        let s: String = chars[..n].iter().collect::<String>() + "…";
        if w(&s) <= max_w {
            return s;
        }
    }
    "…".into()
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::PI;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -PI / 2.0, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, PI / 2.0);
    cr.arc(x + r, y + h - r, r, PI / 2.0, PI);
    cr.arc(x + r, y + r, r, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

/// Colour tokens the lanes need (design tokens, both themes).
#[derive(Clone, Copy)]
struct Palette {
    bg: (f64, f64, f64),
    surface: (f64, f64, f64),
    border: (f64, f64, f64),
    muted: (f64, f64, f64),
    text: (f64, f64, f64),
    blue: (f64, f64, f64),
    red: (f64, f64, f64),
    orange: (f64, f64, f64),
    purple: (f64, f64, f64),
}

impl Palette {
    fn for_theme(theme: Theme) -> Palette {
        match theme {
            Theme::Dark => Palette {
                bg: rgb(0x141a21),
                surface: rgb(0x222b34),
                border: rgb(0x2c3640),
                muted: rgb(0x8a97a5),
                text: rgb(0xe6ebf0),
                blue: rgb(0x2f8fe8),
                red: rgb(0xe5473f),
                orange: rgb(0xf28c28),
                purple: rgb(0xa77bf2),
            },
            Theme::Light => Palette {
                bg: rgb(0xeef1f4),
                surface: rgb(0xffffff),
                border: rgb(0xd6dce3),
                muted: rgb(0x5f6c7a),
                text: rgb(0x1b232c),
                blue: rgb(0x1b7fd6),
                red: rgb(0xe5473f),
                orange: rgb(0xf28c28),
                purple: rgb(0x8a5cf0),
            },
        }
    }

    fn kind(&self, kind: Kind) -> (f64, f64, f64) {
        match kind {
            Kind::Motion => self.orange,
            Kind::Alarm => self.red,
            Kind::Manual => self.purple,
            Kind::Other => (
                self.purple.0 * 0.75 + self.muted.0 * 0.25,
                self.purple.1 * 0.75 + self.muted.1 * 0.25,
                self.purple.2 * 0.75 + self.muted.2 * 0.25,
            ),
        }
    }

    fn set(&self, cr: &cairo::Context, c: (f64, f64, f64)) {
        cr.set_source_rgb(c.0, c.1, c.2);
    }
}

/// `0xRRGGBB` → unit RGB.
pub fn rgb(hex: u32) -> (f64, f64, f64) {
    (
        ((hex >> 16) & 0xff) as f64 / 255.0,
        ((hex >> 8) & 0xff) as f64 / 255.0,
        (hex & 0xff) as f64 / 255.0,
    )
}

fn local_from_secs(secs: i64) -> NaiveDateTime {
    Local
        .timestamp_opt(secs, 0)
        .single()
        .map(|t: DateTime<Local>| t.naive_local())
        .unwrap_or_default()
}

fn local_from_ms(ms: i64) -> NaiveDateTime {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t: DateTime<Local>| t.naive_local())
        .unwrap_or_default()
}

// --- pure geometry / time math ----------------------------------------------------

/// The visible time range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// Unix seconds at the left edge.
    pub start: i64,
    /// Length in seconds.
    pub secs: i64,
}

impl Window {
    /// A window of `secs` whose live edge (97 %) is at `now`.
    pub fn ending_now(now: i64, secs: i64) -> Window {
        let secs = secs.max(60);
        Window {
            start: now - (secs as f64 * LIVE_EDGE_FRAC).round() as i64,
            secs,
        }
    }

    pub fn end(&self) -> i64 {
        self.start + self.secs
    }

    /// Fraction (0–1, unclamped) of `t` across the window.
    pub fn frac(&self, t: i64) -> f64 {
        (t - self.start) as f64 / self.secs as f64
    }

    pub fn frac_f(&self, t: f64) -> f64 {
        (t - self.start as f64) / self.secs as f64
    }

    /// Time at fraction `f`.
    pub fn time_at(&self, f: f64) -> i64 {
        self.start + (f * self.secs as f64).round() as i64
    }

    /// A `new_secs` window with `anchor_t` still at fraction `frac`.
    pub fn zoomed(&self, new_secs: i64, anchor_t: i64, frac: f64) -> Window {
        Window {
            start: anchor_t - (frac * new_secs as f64).round() as i64,
            secs: new_secs,
        }
    }

    /// Tick times: multiples of [`tick_step`] inside the window.
    pub fn ticks(&self) -> Vec<i64> {
        let step = tick_step(self.secs);
        let first = self.start.div_euclid(step) * step;
        let first = if first < self.start {
            first + step
        } else {
            first
        };
        (0..)
            .map(|i| first + i * step)
            .take_while(|&t| t <= self.end())
            .collect()
    }
}

/// Smallest "nice" step (s) giving at most [`TICK_COUNT`] ticks: 10 min for 2 h.
pub fn tick_step(window_secs: i64) -> i64 {
    const STEPS: [i64; 13] = [
        30, 60, 120, 300, 600, 900, 1200, 1800, 3600, 7200, 10800, 14400, 21600,
    ];
    STEPS
        .into_iter()
        .find(|&s| window_secs / s <= TICK_COUNT as i64)
        .unwrap_or(21600)
}

/// Lane height for `n` lanes: 20 px up to 6, then shrinking to 12.
pub fn lane_height(n: usize) -> f64 {
    match n {
        0..=6 => 20.0,
        7..=9 => 16.0,
        10..=12 => 14.0,
        _ => 12.0,
    }
}

/// Position within 3 % of the window of now counts as the live edge.
pub fn is_live_edge(position_secs: i64, now_secs: i64, window_secs: i64) -> bool {
    let tolerance = (window_secs as f64 * LIVE_TOLERANCE_FRAC).round() as i64;
    position_secs >= now_secs - tolerance
}

/// Clip a `(start, stop)` span to the window → fractions, or `None` when outside.
pub fn clip_span(span: (i64, i64), window: &Window) -> Option<(f64, f64)> {
    let (a, b) = span;
    if b <= window.start || a >= window.end() || b < a {
        return None;
    }
    let fa = window.frac(a.max(window.start));
    let fb = window.frac(b.min(window.end()));
    Some((fa.clamp(0.0, 1.0), fb.clamp(0.0, 1.0)))
}

/// Latest start strictly before `pos` (with 1 s slack).
pub fn prev_start(starts: &[i64], pos: i64) -> Option<i64> {
    starts.iter().copied().filter(|&s| s < pos - 1).max()
}

/// Earliest start strictly after `pos` (with 1 s slack).
pub fn next_start(starts: &[i64], pos: i64) -> Option<i64> {
    starts.iter().copied().filter(|&s| s > pos + 1).min()
}

/// The first run of consecutive chunk starts in `missing` (sorted), at most
/// `max_chunks` long, as `(from, to)`.
pub fn first_run(missing: &[i64], max_chunks: i64) -> Option<(i64, i64)> {
    let first = *missing.first()?;
    let mut end = first + CHUNK_SECS;
    for &c in &missing[1..] {
        if c != end || (end - first) / CHUNK_SECS >= max_chunks {
            break;
        }
        end = c + CHUNK_SECS;
    }
    Some((first, end))
}

/// `HH:MM:SS` readout.
pub fn format_readout(t: &NaiveDateTime) -> String {
    t.format("%H:%M:%S").to_string()
}

/// `2026-10-11 SUN`.
pub fn format_date(t: &NaiveDateTime) -> String {
    t.format("%Y-%m-%d %a").to_string().to_uppercase()
}

/// `HH:MM` tick label; midnight shows the date (`31 Oct`) instead.
pub fn format_tick(t: &NaiveDateTime) -> String {
    if t.format("%H:%M:%S").to_string() == "00:00:00" {
        t.format("%-d %b").to_string()
    } else {
        t.format("%H:%M").to_string()
    }
}

/// `5 min`, `2 h`, `24 h`.
pub fn format_zoom(secs: i64) -> String {
    if secs < 3600 {
        format!("{} min", secs / 60)
    } else {
        format!("{} h", secs / 3600)
    }
}

/// `42 s`, `3m 05s`, `1h 03m`.
pub fn format_duration(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs} s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// `1.2 MB` style.
pub fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    const NOW: i64 = 1_760_200_000; // 2025-10-11T16:26:40Z

    #[test]
    fn window_puts_now_at_the_live_edge() {
        let w = Window::ending_now(NOW, 7200);
        assert_eq!(w.secs, 7200);
        assert_eq!(w.start, NOW - 6984);
        assert!((w.frac(NOW) - LIVE_EDGE_FRAC).abs() < 1e-9);
        assert_eq!(w.time_at(LIVE_EDGE_FRAC), NOW);
        assert_eq!(w.time_at(0.0), w.start);
        assert_eq!(w.time_at(1.0), w.end());
    }

    #[test]
    fn zoom_keeps_the_anchor_in_place() {
        let w = Window {
            start: 1000,
            secs: 1000,
        };
        let anchor = w.time_at(0.25);
        let z = w.zoomed(4000, anchor, 0.25);
        assert_eq!(z.secs, 4000);
        assert_eq!(z.time_at(0.25), anchor);
        let z = w.zoomed(500, anchor, 0.25);
        assert_eq!(z.time_at(0.25), anchor);
    }

    #[test]
    fn two_hours_gives_twelve_ten_minute_ticks() {
        assert_eq!(tick_step(7200), 600);
        let w = Window::ending_now(NOW, 7200);
        let ticks = w.ticks();
        assert_eq!(ticks.len(), 12);
        assert!(ticks.iter().all(|t| t % 600 == 0));
        assert!(ticks.windows(2).all(|p| p[1] - p[0] == 600));
        assert!(ticks[0] >= w.start && *ticks.last().unwrap() <= w.end());
    }

    #[test]
    fn tick_steps_stay_under_twelve_at_every_zoom() {
        for secs in ZOOM_LEVELS {
            let step = tick_step(secs);
            assert!(secs / step <= TICK_COUNT as i64, "{secs}/{step}");
            let n = Window::ending_now(NOW, secs).ticks().len();
            assert!((5..=TICK_COUNT + 1).contains(&n), "{secs}: {n} ticks");
        }
        assert_eq!(tick_step(300), 30);
        assert_eq!(tick_step(3600), 300);
        assert_eq!(tick_step(86400), 7200);
        assert_eq!(ZOOM_LEVELS[DEFAULT_ZOOM], 7200);
        assert_eq!(ZOOM_LEVELS[0], 300);
        assert_eq!(*ZOOM_LEVELS.last().unwrap(), 86400);
    }

    #[test]
    fn live_edge_is_three_percent_of_the_window() {
        assert!(is_live_edge(NOW, NOW, 7200));
        assert!(is_live_edge(NOW - 216, NOW, 7200));
        assert!(!is_live_edge(NOW - 217, NOW, 7200));
        assert!(is_live_edge(NOW + 5, NOW, 7200));
        let w = Window::ending_now(NOW, 7200);
        assert!(is_live_edge(w.time_at(0.97), NOW, w.secs));
        assert!(!is_live_edge(w.time_at(0.90), NOW, w.secs));
    }

    #[test]
    fn spans_clip_to_the_window() {
        let w = Window {
            start: 1000,
            secs: 1000,
        };
        assert_eq!(clip_span((0, 500), &w), None);
        assert_eq!(clip_span((2500, 3000), &w), None);
        assert_eq!(clip_span((1250, 1750), &w), Some((0.25, 0.75)));
        assert_eq!(clip_span((500, 1500), &w), Some((0.0, 0.5)));
        assert_eq!(clip_span((1500, 9000), &w), Some((0.5, 1.0)));
        assert_eq!(clip_span((1800, 1700), &w), None);
    }

    #[test]
    fn event_navigation_skips_the_current_one() {
        let starts = [100, 300, 500];
        assert_eq!(next_start(&starts, 100), Some(300));
        assert_eq!(next_start(&starts, 310), Some(500));
        assert_eq!(next_start(&starts, 600), None);
        assert_eq!(prev_start(&starts, 310), Some(300));
        assert_eq!(prev_start(&starts, 301), Some(100));
        assert_eq!(prev_start(&starts, 300), Some(100));
        assert_eq!(prev_start(&starts, 600), Some(500));
        assert_eq!(prev_start(&starts, 100), None);
    }

    #[test]
    fn geometry_hit_testing() {
        let g = Geometry {
            width: 1000.0,
            height: 200.0,
            name_w: 100.0,
            ruler_h: 20.0,
            lane_h: 20.0,
            lanes: 4,
        };
        assert_eq!(g.axis_w(), 900.0);
        assert_eq!(g.axis_x(0.5), 550.0);
        assert!((g.frac_at(550.0) - 0.5).abs() < 1e-9);
        assert_eq!(g.lane_at(10.0), None);
        assert_eq!(g.lane_at(20.0), Some(0));
        assert_eq!(g.lane_at(39.9), Some(0));
        assert_eq!(g.lane_at(40.0), Some(1));
        assert_eq!(g.lane_at(99.9), Some(3));
        assert_eq!(g.lane_at(100.0), None);
        assert_eq!(g.lane_y(2), 60.0);
    }

    #[test]
    fn lane_heights_shrink_with_many_cameras() {
        assert_eq!(lane_height(1), 20.0);
        assert_eq!(lane_height(6), 20.0);
        assert_eq!(lane_height(9), 16.0);
        assert_eq!(lane_height(16), 12.0);
        // 16 lanes + ruler + controls stay well under 40 % of a 600 px stage
        let total = CONTROLS_H as f64 + RULER_H + 16.0 * lane_height(16) + PAD_BOTTOM + 8.0;
        assert!(total <= 280.0, "{total}");
    }

    #[test]
    fn kinds_map_recording_modes_and_event_center_types() {
        assert_eq!(Kind::from_recording_mode(1), Kind::Motion);
        assert_eq!(Kind::from_recording_mode(2), Kind::Alarm);
        assert_eq!(Kind::from_recording_mode(3), Kind::Manual);
        assert_eq!(Kind::from_recording_mode(0), Kind::Other);
        assert_eq!(Kind::from_recording_mode(99), Kind::Other);
        assert_eq!(Kind::from_event_center(10), Kind::Motion);
        assert_eq!(Kind::from_event_center(19), Kind::Alarm);
        assert_eq!(Kind::from_event_center(12), Kind::Alarm);
        assert_eq!(Kind::from_event_center(11), Kind::Other);
        assert_eq!(Kind::from_event_center(1), Kind::Other);
        for k in Kind::ALL {
            assert_eq!(Kind::from_key(k.key()), Some(k));
        }
        assert_eq!(Kind::from_key("bogus"), None);
    }

    #[test]
    fn filter_round_trips_through_the_session() {
        let mut session = ss_config::Session::default();
        let f = Filter::from_config(&session);
        assert_eq!(f, Filter::all());
        let mut f = Filter::all();
        f.set(Kind::Alarm, false);
        f.recordings = false;
        f.write_config(&mut session);
        assert_eq!(
            session.timeline_event_types,
            vec!["motion", "manual", "other"]
        );
        assert!(!session.timeline_show_recordings);
        assert_eq!(Filter::from_config(&session), f);
    }

    #[test]
    fn cache_tracks_missing_and_stale_chunks() {
        let now = 10 * CHUNK_SECS + 100;
        let mut cache = Cache::default();
        cache.reset_for(&[1, 2]);
        // 3 hours back from now, nothing cached: four chunks (7..10)
        let missing = cache.missing(now - 3 * CHUNK_SECS, now + CHUNK_SECS, now);
        assert_eq!(
            missing,
            vec![
                7 * CHUNK_SECS,
                8 * CHUNK_SECS,
                9 * CHUNK_SECS,
                10 * CHUNK_SECS
            ]
        );
        assert_eq!(
            first_run(&missing, 24),
            Some((7 * CHUNK_SECS, 11 * CHUNK_SECS))
        );
        assert_eq!(
            first_run(&missing, 2),
            Some((7 * CHUNK_SECS, 9 * CHUNK_SECS))
        );
        assert_eq!(first_run(&[], 24), None);
        // a run breaks at a gap
        assert_eq!(
            first_run(&[0, CHUNK_SECS, 3 * CHUNK_SECS], 24),
            Some((0, 2 * CHUNK_SECS))
        );

        let fetched = Fetched {
            recordings: HashMap::from([(1, vec![(7 * CHUNK_SECS + 10, 9 * CHUNK_SECS + 10)])]),
            events: vec![EventMark {
                camera_id: 1,
                kind: Kind::Motion,
                start: 8 * CHUNK_SECS + 5,
                end: 8 * CHUNK_SECS + 47,
            }],
        };
        cache.store(7 * CHUNK_SECS, 11 * CHUNK_SECS, &fetched, now);
        assert_eq!(cache.chunk_count(), 4);
        assert!(cache.missing(now - 3 * CHUNK_SECS, now, now).is_empty());
        // the span is read back merged, clipped to the asked range
        assert_eq!(
            cache.recordings(1, 8 * CHUNK_SECS, 8 * CHUNK_SECS + 100),
            vec![(7 * CHUNK_SECS + 10, 9 * CHUNK_SECS + 10)]
        );
        assert!(cache.recordings(2, 0, now).is_empty());
        assert_eq!(cache.events(0, now).len(), 1);
        assert_eq!(cache.event_count(), 1);
        // only the chunk that covered "now" goes stale
        let later = now + REFRESH_SECS;
        assert_eq!(
            cache.missing(now - 3 * CHUNK_SECS, now, later),
            vec![10 * CHUNK_SECS]
        );
        // a different camera set drops everything
        cache.reset_for(&[1, 2, 3]);
        assert_eq!(cache.chunk_count(), 0);
    }

    #[test]
    fn formats_match_the_spec() {
        let t = NaiveDate::from_ymd_opt(2026, 10, 11)
            .unwrap()
            .and_hms_opt(15, 3, 9)
            .unwrap();
        assert_eq!(format_readout(&t), "15:03:09");
        assert_eq!(format_date(&t), "2026-10-11 SUN");
        assert_eq!(format_tick(&t), "15:03");
        let midnight = NaiveDate::from_ymd_opt(2026, 10, 31)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        assert_eq!(format_tick(&midnight), "31 Oct");
        assert_eq!(format_zoom(300), "5 min");
        assert_eq!(format_zoom(7200), "2 h");
        assert_eq!(format_zoom(86400), "24 h");
        assert_eq!(format_duration(42), "42 s");
        assert_eq!(format_duration(185), "3m 05s");
        assert_eq!(format_duration(3780), "1h 03m");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1_250_000), "1.2 MB");
    }

    #[test]
    fn rgb_unpacks_hex() {
        let (r, g, b) = rgb(0x2f8fe8);
        assert!((r - 47.0 / 255.0).abs() < 1e-9);
        assert!((g - 143.0 / 255.0).abs() < 1e-9);
        assert!((b - 232.0 / 255.0).abs() < 1e-9);
        assert_eq!(rgb(0xffffff), (1.0, 1.0, 1.0));
    }
}
