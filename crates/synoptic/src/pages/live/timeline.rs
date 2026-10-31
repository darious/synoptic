//! The 112 px timeline strip under the Live View grid: a 44 px control row
//! and a Cairo-drawn ruler (2 h window ending at now, 12 ticks, presence
//! rows, live edge, playhead). Geometry and time math live in pure functions
//! at the bottom so they can be unit-tested on a headless machine.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use gtk::cairo;
use gtk::glib;
use gtk::prelude::*;

use super::grid::{Grid, Mode, Playback, SPEEDS};
use crate::icons;
use crate::services::Services;
use crate::state::{Store, Theme};
use crate::widgets::controls::{divider_v, label, set_class, spacer};

// --- constants (design/handoff/README.md §1 "Timeline strip") ------------------

/// Default window: 2 h.
#[cfg(test)]
pub const DEFAULT_WINDOW_SECS: i64 = 2 * 3600;
/// The live edge sits at 97 % of the ruler width.
pub const LIVE_EDGE_FRAC: f64 = 0.97;
/// Position ≥ now − window × 3 % counts as live.
pub const LIVE_TOLERANCE_FRAC: f64 = 0.03;
/// Target number of tick marks.
pub const TICK_COUNT: usize = 12;
/// Zoom levels (window length in seconds); index 3 is the default 2 h.
pub const ZOOM_LEVELS: [i64; 7] = [900, 1800, 3600, 7200, 14400, 28800, 86400];
pub const DEFAULT_ZOOM: usize = 3;
/// Presence rows: refetch when the data is older than this.
const PRESENCE_REFRESH_SECS: i64 = 60;

// --- widget -------------------------------------------------------------------

#[derive(Default)]
struct Presence {
    /// camera id → merged recording spans (unix s)
    recordings: HashMap<i64, Vec<(i64, i64)>>,
    /// camera id → event spans (unix s)
    events: HashMap<i64, Vec<(i64, i64)>>,
    /// range the data covers
    from: i64,
    to: i64,
    /// when it was fetched (unix s)
    fetched_at: i64,
    ids: Vec<i64>,
}

struct Timeline {
    store: Rc<Store>,
    services: Rc<Services>,
    grid: Rc<Grid>,
    readout: gtk::Label,
    date_label: gtk::Label,
    tracking_box: gtk::Box,
    tracking_cam: gtk::Label,
    pause_button: gtk::Button,
    pause_icon: gtk::Image,
    forward_button: gtk::Button,
    next_button: gtk::Button,
    speed_dropdown: gtk::DropDown,
    live_button: gtk::Button,
    live_label: gtk::Label,
    ruler: gtk::DrawingArea,
    zoom: Cell<usize>,
    presence: RefCell<Presence>,
    fetching: Cell<bool>,
    fetch_generation: Cell<u64>,
    /// Last position shown in History (unix ms), kept while a seek is in flight.
    last_position_ms: Cell<Option<i64>>,
}

pub fn build(store: &Rc<Store>, services: &Rc<Services>, grid: &Rc<Grid>) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("timeline");
    root.set_size_request(-1, 112);

    // ---- control row ----
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    controls.add_css_class("controls");
    controls.set_size_request(-1, 44);

    let readout_block = gtk::Box::new(gtk::Orientation::Vertical, 0);
    readout_block.set_size_request(150, -1);
    readout_block.set_valign(gtk::Align::Center);
    let readout = label("--:--:--", &["readout", "live"]);
    let date_label = label("", &["readout-date"]);
    readout_block.append(&readout);
    readout_block.append(&date_label);
    controls.append(&readout_block);

    let filter_button = icons::bordered_button("filter_list", "icon-30", "Filter events");
    let download_button = icons::bordered_button("download", "icon-30", "Download");
    let datetime_button = icons::bordered_button("calendar_month", "icon-30", "Date / time");
    let zoom_out_button = icons::bordered_button("zoom_out", "icon-30", "Zoom out");
    let zoom_in_button = icons::bordered_button("zoom_in", "icon-30", "Zoom in");
    for b in [
        &filter_button,
        &download_button,
        &datetime_button,
        &zoom_out_button,
        &zoom_in_button,
    ] {
        controls.append(b);
    }
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
    let back_button = icons::bordered_button("replay_10", "icon-30", "Back 10 s");
    let pause_button = gtk::Button::new();
    pause_button.add_css_class("icon-30");
    pause_button.set_tooltip_text(Some("Pause"));
    pause_button.set_valign(gtk::Align::Center);
    let pause_icon = icons::image("pause", 17);
    pause_button.set_child(Some(&pause_icon));
    let forward_button = icons::bordered_button("forward_10", "icon-30", "Forward 10 s");
    let next_button = icons::bordered_button("skip_next", "icon-30", "Next event");

    let labels: Vec<&str> = SPEEDS.iter().map(|(l, _)| *l).collect();
    let speed_dropdown = gtk::DropDown::from_strings(&labels);
    speed_dropdown.add_css_class("speed");
    speed_dropdown.set_valign(gtk::Align::Center);
    speed_dropdown.set_tooltip_text(Some("Playback speed"));

    let live_button = gtk::Button::new();
    live_button.add_css_class("live-button");
    live_button.set_valign(gtk::Align::Center);
    let live_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let live_dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    live_dot.add_css_class("live-dot");
    live_dot.set_valign(gtk::Align::Center);
    let live_label = gtk::Label::new(Some("Live"));
    live_row.append(&live_dot);
    live_row.append(&live_label);
    live_button.set_child(Some(&live_row));

    for b in [
        &prev_button,
        &back_button,
        &pause_button,
        &forward_button,
        &next_button,
    ] {
        controls.append(b);
    }
    controls.append(&speed_dropdown);
    controls.append(&live_button);
    root.append(&controls);

    // ---- ruler ----
    let ruler = gtk::DrawingArea::new();
    ruler.add_css_class("ruler");
    ruler.set_hexpand(true);
    ruler.set_vexpand(true);
    ruler.set_margin_start(12);
    ruler.set_margin_end(12);
    ruler.set_margin_bottom(10);
    root.append(&ruler);

    let tl = Rc::new(Timeline {
        store: store.clone(),
        services: services.clone(),
        grid: grid.clone(),
        readout,
        date_label,
        tracking_box,
        tracking_cam,
        pause_button,
        pause_icon,
        forward_button: forward_button.clone(),
        next_button: next_button.clone(),
        speed_dropdown,
        live_button,
        live_label,
        ruler,
        zoom: Cell::new(DEFAULT_ZOOM),
        presence: RefCell::new(Presence::default()),
        fetching: Cell::new(false),
        fetch_generation: Cell::new(0),
        last_position_ms: Cell::new(None),
    });

    // ---- wiring ----
    for (button, what) in [
        (&filter_button, "Event filter"),
        (&download_button, "Download"),
        (&datetime_button, "Date / time picker"),
    ] {
        let store = store.clone();
        button.connect_clicked(move |_| store.toast(format!("{what} not implemented")));
    }
    {
        let w = Rc::downgrade(&tl);
        zoom_out_button.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.zoom_by(1);
            }
        });
    }
    {
        let w = Rc::downgrade(&tl);
        zoom_in_button.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.zoom_by(-1);
            }
        });
    }
    {
        let w = Rc::downgrade(&tl);
        prev_button.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.jump_event(-1);
            }
        });
    }
    {
        let w = Rc::downgrade(&tl);
        next_button.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.jump_event(1);
            }
        });
    }
    {
        let w = Rc::downgrade(&tl);
        back_button.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.step(-10);
            }
        });
    }
    {
        let w = Rc::downgrade(&tl);
        forward_button.connect_clicked(move |_| {
            if let Some(t) = w.upgrade() {
                t.step(10);
            }
        });
    }
    {
        let grid = grid.clone();
        tl.pause_button.connect_clicked(move |_| {
            let paused = grid.playback().paused;
            grid.set_paused(!paused);
        });
    }
    {
        let grid = grid.clone();
        tl.speed_dropdown.connect_selected_notify(move |dd| {
            let i = dd.selected();
            if i != gtk::INVALID_LIST_POSITION {
                grid.set_speed_index(i as usize);
            }
        });
    }
    {
        let grid = grid.clone();
        tl.live_button.connect_clicked(move |_| {
            if grid.playback().mode == Mode::History {
                grid.go_live();
            }
        });
    }

    // ruler: draw + click-to-seek
    {
        let w = Rc::downgrade(&tl);
        tl.ruler.set_draw_func(move |_, cr, width, height| {
            if let Some(t) = w.upgrade() {
                t.draw(cr, width as f64, height as f64);
            }
        });
    }
    {
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        let w = Rc::downgrade(&tl);
        click.connect_released(move |gesture, _, x, _| {
            let Some(t) = w.upgrade() else { return };
            let width = gesture.widget().map(|w| w.width()).unwrap_or(1).max(1) as f64;
            t.click_at(x / width);
        });
        tl.ruler.add_controller(click);
    }

    // grid state → controls
    {
        let w = Rc::downgrade(&tl);
        grid.subscribe(move || {
            if let Some(t) = w.upgrade() {
                t.refresh_controls();
                t.ruler.queue_draw();
                t.maybe_fetch();
            }
        });
    }
    // clock + redraw
    {
        let w = Rc::downgrade(&tl);
        glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            let Some(t) = w.upgrade() else {
                return glib::ControlFlow::Break;
            };
            t.refresh_readout();
            t.ruler.queue_draw();
            t.maybe_fetch();
            glib::ControlFlow::Continue
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

    fn window(&self) -> Window {
        Window::ending_now(Self::now_secs(), ZOOM_LEVELS[self.zoom.get()])
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
        let ms = self.position_ms(&pb);
        let t = local_from_ms(ms);
        self.readout.set_text(&format_readout(&t));
        self.date_label.set_text(&format_date(&t));
        let history = pb.mode == Mode::History;
        set_class(&self.readout, "history", history);
        set_class(&self.readout, "live", !history);
    }

    fn refresh_controls(&self) {
        let pb = self.grid.playback();
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
        self.pause_button
            .set_tooltip_text(Some(if pb.paused { "Play" } else { "Pause" }));

        self.forward_button.set_sensitive(!at_edge);
        self.next_button.set_sensitive(!at_edge);
        self.speed_dropdown.set_sensitive(!at_edge);
        if self.speed_dropdown.selected() != pb.speed_index as u32 {
            self.speed_dropdown.set_selected(pb.speed_index as u32);
        }

        set_class(&self.live_button, "live-edge", at_edge);
        set_class(&self.live_button, "primary", !at_edge);
        self.live_label
            .set_text(if at_edge { "Live" } else { "Go live" });
        self.refresh_readout();
    }

    fn zoom_by(self: &Rc<Self>, delta: i32) {
        let z = (self.zoom.get() as i32 + delta).clamp(0, ZOOM_LEVELS.len() as i32 - 1) as usize;
        if self.zoom.replace(z) != z {
            self.presence.borrow_mut().fetched_at = 0;
            self.ruler.queue_draw();
            self.maybe_fetch();
        }
    }

    fn click_at(&self, frac: f64) {
        let window = self.window();
        let t = window.time_at(frac.clamp(0.0, 1.0));
        let now = Self::now_secs();
        if is_live_edge(t, now, window.secs) {
            self.grid.go_live();
        } else {
            self.last_position_ms.set(Some(t * 1000));
            self.grid.seek(t);
        }
    }

    /// Back/Forward 10 s.
    fn step(&self, delta_secs: i64) {
        let pb = self.grid.playback();
        let pos = self.position_ms(&pb) / 1000 + delta_secs;
        let now = Self::now_secs();
        if is_live_edge(pos, now, self.window().secs) && delta_secs > 0 {
            self.grid.go_live();
        } else {
            self.last_position_ms.set(Some(pos * 1000));
            self.grid.seek(pos.min(now));
        }
    }

    /// Previous/Next event: boundaries of the tracked camera's event spans
    /// (or its recordings when the NAS reports no events).
    fn jump_event(&self, direction: i32) {
        let pb = self.grid.playback();
        let pos = self.position_ms(&pb) / 1000;
        let Some(cam) = self.grid.tracked_camera() else {
            return;
        };
        let target = {
            let p = self.presence.borrow();
            let spans = p
                .events
                .get(&cam.id)
                .filter(|v| !v.is_empty())
                .or_else(|| p.recordings.get(&cam.id))
                .cloned()
                .unwrap_or_default();
            if direction < 0 {
                prev_event(&spans, pos)
            } else {
                next_event(&spans, pos)
            }
        };
        match target {
            Some(t) => {
                self.last_position_ms.set(Some(t * 1000));
                self.grid.seek(t);
            }
            None => self.store.toast(if direction < 0 {
                "No earlier event in view"
            } else {
                "No later event in view"
            }),
        }
    }

    // --- presence data -------------------------------------------------------

    fn maybe_fetch(self: &Rc<Self>) {
        if self.fetching.get() {
            return;
        }
        let Some(client) = self.services.client() else {
            return;
        };
        let ids = self.grid.camera_ids();
        if ids.is_empty() {
            return;
        }
        let window = self.window();
        let now = Self::now_secs();
        {
            let p = self.presence.borrow();
            let fresh = now - p.fetched_at < PRESENCE_REFRESH_SECS;
            let covers = p.from <= window.start && p.to >= window.end().min(now);
            if fresh && covers && p.ids == ids {
                return;
            }
        }
        // fetch a little more than the window so small zoom/scroll changes hit the cache
        let from = window.start - window.secs / 4;
        let to = now + 60;
        let has_events = client.has_api(ss_api::event::EVENT_CENTER_API);
        let ids_task = ids.clone();
        self.fetching.set(true);
        let generation = self.fetch_generation.get() + 1;
        self.fetch_generation.set(generation);
        let fut = self.services.run(async move {
            let recs = client.enum_interval(&ids_task, from, to).await;
            let events = if has_events {
                client
                    .list_event_center_events(&ids_task, from, to)
                    .await
                    .ok()
            } else {
                None
            };
            (recs, events)
        });
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let (recs, events) = fut.await;
            let Some(t) = w.upgrade() else { return };
            t.fetching.set(false);
            if t.fetch_generation.get() != generation {
                return;
            }
            let mut p = t.presence.borrow_mut();
            match recs {
                Ok(cams) => {
                    p.recordings.clear();
                    for c in cams {
                        p.recordings
                            .entry(c.camera_id)
                            .or_default()
                            .extend(c.presence_spans());
                    }
                    for v in p.recordings.values_mut() {
                        *v = ss_api::merge_intervals(v.drain(..));
                    }
                }
                Err(e) => tracing::warn!("enum_interval: {e}"),
            }
            if let Some(evs) = events {
                p.events.clear();
                for e in evs {
                    p.events
                        .entry(e.camera_id)
                        .or_default()
                        .push((e.start_time, e.start_time + e.duration.max(1)));
                }
                for v in p.events.values_mut() {
                    *v = ss_api::merge_intervals(v.drain(..));
                }
            }
            p.from = from;
            p.to = to;
            p.fetched_at = Self::now_secs();
            p.ids = ids;
            drop(p);
            t.ruler.queue_draw();
        });
    }

    // --- drawing -------------------------------------------------------------

    fn draw(&self, cr: &cairo::Context, width: f64, height: f64) {
        let palette = Palette::for_theme(self.store.get().theme);
        let window = self.window();
        let now = Self::now_secs();
        let pb = self.grid.playback();
        let history = pb.mode == Mode::History;
        let pos_ms = self.position_ms(&pb);

        // frame: bg + 1 px border, radius 4, clip to it
        rounded_rect(cr, 0.5, 0.5, width - 1.0, height - 1.0, 4.0);
        palette.set(cr, palette.bg);
        let _ = cr.fill_preserve();
        palette.set(cr, palette.border);
        cr.set_line_width(1.0);
        let _ = cr.stroke_preserve();
        cr.clip();

        cr.select_font_face(
            "IBM Plex Mono",
            cairo::FontSlant::Normal,
            cairo::FontWeight::Normal,
        );

        // ticks + labels
        cr.set_font_size(11.0);
        for tick in window.ticks() {
            let x = window.x(tick, width).round() + 0.5;
            palette.set(cr, palette.border);
            cr.move_to(x, 0.0);
            cr.line_to(x, height);
            let _ = cr.stroke();
            palette.set(cr, palette.muted);
            cr.move_to(x + 4.0, 6.0 + 9.0);
            let _ = cr.show_text(&format_tick(&local_from_secs(tick)));
        }

        // presence rows
        let tracked = self.grid.tracked_camera().map(|c| c.id);
        let ids = self.grid.camera_ids();
        {
            let p = self.presence.borrow();
            // row A: own camera, top 30, h 14, surface bg
            palette.set(cr, palette.surface);
            cr.rectangle(0.0, 30.0, width, 14.0);
            let _ = cr.fill();
            if let Some(id) = tracked {
                if let Some(spans) = p.recordings.get(&id) {
                    cr.set_source_rgba(palette.blue.0, palette.blue.1, palette.blue.2, 0.55);
                    draw_spans(cr, &window, spans, width, 30.0, 14.0);
                }
                if let Some(spans) = p.events.get(&id) {
                    palette.set(cr, palette.orange);
                    draw_spans(cr, &window, spans, width, 30.0, 14.0);
                }
            }
            // row B: any camera, top 46, h 8
            let others: Vec<(i64, i64)> = ids
                .iter()
                .flat_map(|id| p.recordings.get(id).cloned().unwrap_or_default())
                .collect();
            if !others.is_empty() {
                cr.set_source_rgba(palette.blue.0, palette.blue.1, palette.blue.2, 0.35);
                draw_spans(
                    cr,
                    &window,
                    &ss_api::merge_intervals(others),
                    width,
                    46.0,
                    8.0,
                );
            }
            let other_events: Vec<(i64, i64)> = ids
                .iter()
                .flat_map(|id| p.events.get(id).cloned().unwrap_or_default())
                .collect();
            if !other_events.is_empty() {
                cr.set_source_rgba(palette.orange.0, palette.orange.1, palette.orange.2, 0.7);
                draw_spans(
                    cr,
                    &window,
                    &ss_api::merge_intervals(other_events),
                    width,
                    46.0,
                    8.0,
                );
            }
        }

        // live edge: 1 px dashed muted at 97 %
        {
            let x = (width * LIVE_EDGE_FRAC).round() + 0.5;
            palette.set(cr, palette.muted);
            cr.set_dash(&[3.0, 3.0], 0.0);
            cr.move_to(x, 0.0);
            cr.line_to(x, height);
            let _ = cr.stroke();
            cr.set_dash(&[], 0.0);
        }

        // legend bottom-right
        {
            cr.set_font_size(10.0);
            let items = [("recording", palette.blue), ("event", palette.orange)];
            let mut x = width - 8.0;
            for (text, colour) in items.iter().rev() {
                let ext = cr.text_extents(text).map(|e| e.x_advance()).unwrap_or(40.0);
                x -= ext;
                palette.set(cr, palette.muted);
                cr.move_to(x, height - 6.0);
                let _ = cr.show_text(text);
                x -= 12.0;
                palette.set(cr, *colour);
                cr.rectangle(x, height - 13.0, 8.0, 8.0);
                let _ = cr.fill();
                x -= 12.0;
            }
        }

        // playhead
        let play_t = if history {
            pos_ms as f64 / 1000.0
        } else {
            now as f64
        };
        let x = window.x_f(play_t, width).round();
        let colour = if history { palette.blue } else { palette.red };
        palette.set(cr, colour);
        cr.rectangle(x - 1.0, 0.0, 2.0, height);
        let _ = cr.fill();

        if history {
            // bubble at top 2 centred on the playhead
            cr.set_font_size(10.0);
            cr.select_font_face(
                "IBM Plex Mono",
                cairo::FontSlant::Normal,
                cairo::FontWeight::Bold,
            );
            let text = format_readout(&local_from_ms(pos_ms));
            let tw = cr
                .text_extents(&text)
                .map(|e| e.x_advance())
                .unwrap_or(50.0);
            let bw = tw + 10.0;
            let bx = (x - bw / 2.0).clamp(2.0, (width - bw - 2.0).max(2.0));
            rounded_rect(cr, bx, 2.0, bw, 16.0, 3.0);
            palette.set(cr, palette.blue);
            let _ = cr.fill();
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.move_to(bx + 5.0, 2.0 + 12.0);
            let _ = cr.show_text(&text);
        }
    }
}

fn draw_spans(
    cr: &cairo::Context,
    window: &Window,
    spans: &[(i64, i64)],
    width: f64,
    y: f64,
    h: f64,
) {
    for &(a, b) in spans {
        if let Some((fa, fb)) = clip_span((a, b), window) {
            let x0 = fa * width;
            let x1 = (fb * width).max(x0 + 1.0);
            cr.rectangle(x0, y, x1 - x0, h);
        }
    }
    let _ = cr.fill();
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

/// Colour tokens the ruler needs (design tokens, both themes).
#[derive(Clone, Copy)]
struct Palette {
    bg: (f64, f64, f64),
    surface: (f64, f64, f64),
    border: (f64, f64, f64),
    muted: (f64, f64, f64),
    blue: (f64, f64, f64),
    red: (f64, f64, f64),
    orange: (f64, f64, f64),
}

impl Palette {
    fn for_theme(theme: Theme) -> Palette {
        match theme {
            Theme::Dark => Palette {
                bg: rgb(0x141a21),
                surface: rgb(0x222b34),
                border: rgb(0x2c3640),
                muted: rgb(0x8a97a5),
                blue: rgb(0x2f8fe8),
                red: rgb(0xe5473f),
                orange: rgb(0xf28c28),
            },
            Theme::Light => Palette {
                bg: rgb(0xeef1f4),
                surface: rgb(0xffffff),
                border: rgb(0xd6dce3),
                muted: rgb(0x5f6c7a),
                blue: rgb(0x1b7fd6),
                red: rgb(0xe5473f),
                orange: rgb(0xf28c28),
            },
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

/// The ruler's visible time range.
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

    /// Pixel x of `t` for a ruler `width` px wide.
    pub fn x(&self, t: i64, width: f64) -> f64 {
        self.frac(t) * width
    }

    pub fn x_f(&self, t: f64, width: f64) -> f64 {
        self.frac_f(t) * width
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
    const STEPS: [i64; 11] = [60, 120, 300, 600, 900, 1200, 1800, 3600, 7200, 10800, 14400];
    STEPS
        .into_iter()
        .find(|&s| window_secs / s <= TICK_COUNT as i64)
        .unwrap_or(14400)
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

/// Latest span start strictly before `pos` (with 1 s slack).
pub fn prev_event(spans: &[(i64, i64)], pos: i64) -> Option<i64> {
    spans.iter().map(|s| s.0).filter(|&s| s < pos - 1).max()
}

/// Earliest span start strictly after `pos` (with 1 s slack).
pub fn next_event(spans: &[(i64, i64)], pos: i64) -> Option<i64> {
    spans.iter().map(|s| s.0).filter(|&s| s > pos + 1).min()
}

/// `HH:MM:SS` readout.
pub fn format_readout(t: &NaiveDateTime) -> String {
    t.format("%H:%M:%S").to_string()
}

/// `2026-10-11 SUN`.
pub fn format_date(t: &NaiveDateTime) -> String {
    t.format("%Y-%m-%d %a").to_string().to_uppercase()
}

/// `HH:MM` tick label.
pub fn format_tick(t: &NaiveDateTime) -> String {
    t.format("%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    const NOW: i64 = 1_760_200_000; // 2025-10-11T16:26:40Z

    #[test]
    fn window_puts_now_at_the_live_edge() {
        let w = Window::ending_now(NOW, DEFAULT_WINDOW_SECS);
        assert_eq!(w.secs, 7200);
        assert_eq!(w.start, NOW - 6984);
        assert!((w.frac(NOW) - LIVE_EDGE_FRAC).abs() < 1e-9);
        assert_eq!(w.time_at(LIVE_EDGE_FRAC), NOW);
        assert_eq!(w.time_at(0.0), w.start);
        assert_eq!(w.time_at(1.0), w.end());
        assert!((w.x(NOW, 1000.0) - 970.0).abs() < 1e-6);
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
    fn tick_steps_stay_under_twelve() {
        for secs in ZOOM_LEVELS {
            let step = tick_step(secs);
            assert!(secs / step <= TICK_COUNT as i64, "{secs}/{step}");
            let n = Window::ending_now(NOW, secs).ticks().len();
            assert!((6..=TICK_COUNT + 1).contains(&n), "{secs}: {n} ticks");
        }
        assert_eq!(tick_step(3600), 300);
        assert_eq!(tick_step(86400), 7200);
    }

    #[test]
    fn live_edge_is_three_percent_of_the_window() {
        assert!(is_live_edge(NOW, NOW, 7200));
        assert!(is_live_edge(NOW - 216, NOW, 7200));
        assert!(!is_live_edge(NOW - 217, NOW, 7200));
        assert!(is_live_edge(NOW + 5, NOW, 7200));
        // a ruler click at the dashed line is live; just left of it is history
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
        let spans = [(100, 150), (300, 320), (500, 560)];
        assert_eq!(next_event(&spans, 100), Some(300));
        assert_eq!(next_event(&spans, 310), Some(500));
        assert_eq!(next_event(&spans, 600), None);
        // inside an event: back to its start; at its start: the one before
        assert_eq!(prev_event(&spans, 310), Some(300));
        assert_eq!(prev_event(&spans, 301), Some(100));
        assert_eq!(prev_event(&spans, 300), Some(100));
        assert_eq!(prev_event(&spans, 600), Some(500));
        assert_eq!(prev_event(&spans, 100), None);
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
