//! PTZ popover for a Live View slot: direction pad (hold to move, release
//! to stop), home and stop, zoom and focus (hold), and the camera's presets
//! and patrols. Talks to `SYNO.SurveillanceStation.PTZ` through
//! [`ss_api::Client`]'s `ptz_*` methods.
//!
//! "Hold" is read from the button's `:active` state flag (set on press,
//! cleared on release or cancel), which is the one signal GTK 4 still gives
//! for press/release on a `gtk::Button`. A `clicked` that arrives without a
//! press (keyboard activation, the headless driver) becomes a short nudge:
//! start, then stop after [`NUDGE`].

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use ss_api::{Camera, Client, FocusControl, PtzDirection, ZoomControl};

use crate::icons;
use crate::services::Services;
use crate::state::Store;
use crate::widgets::controls::{divider_h, label};
use crate::widgets::popovers::menu_row;

/// How long a click without a press moves the camera.
pub const NUDGE: Duration = Duration::from_millis(400);

/// A direction on the pad; diagonals are two simultaneous `PTZ.Move`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pad {
    Up,
    Down,
    Left,
    Right,
    UpLeft,
    UpRight,
    DownLeft,
    DownRight,
}

impl Pad {
    pub const ALL: [Pad; 8] = [
        Pad::UpLeft,
        Pad::Up,
        Pad::UpRight,
        Pad::Left,
        Pad::Right,
        Pad::DownLeft,
        Pad::Down,
        Pad::DownRight,
    ];

    /// `(start, stop)` wire directions, in the order they are sent.
    pub fn moves(self) -> (Vec<PtzDirection>, Vec<PtzDirection>) {
        use PtzDirection::*;
        match self {
            Pad::Up => (vec![UpStart], vec![UpStop]),
            Pad::Down => (vec![DownStart], vec![DownStop]),
            Pad::Left => (vec![LeftStart], vec![LeftStop]),
            Pad::Right => (vec![RightStart], vec![RightStop]),
            Pad::UpLeft => (vec![UpStart, LeftStart], vec![UpStop, LeftStop]),
            Pad::UpRight => (vec![UpStart, RightStart], vec![UpStop, RightStop]),
            Pad::DownLeft => (vec![DownStart, LeftStart], vec![DownStop, LeftStop]),
            Pad::DownRight => (vec![DownStart, RightStart], vec![DownStop, RightStop]),
        }
    }

    /// Material icon and widget-name suffix.
    pub fn icon(self) -> (&'static str, &'static str) {
        match self {
            Pad::Up => ("arrow_upward", "up"),
            Pad::Down => ("arrow_downward", "down"),
            Pad::Left => ("arrow_back", "left"),
            Pad::Right => ("arrow_forward", "right"),
            Pad::UpLeft => ("north_west", "up-left"),
            Pad::UpRight => ("north_east", "up-right"),
            Pad::DownLeft => ("south_west", "down-left"),
            Pad::DownRight => ("south_east", "down-right"),
        }
    }

    /// `(column, row)` in the 3×3 pad.
    pub fn cell(self) -> (i32, i32) {
        match self {
            Pad::UpLeft => (0, 0),
            Pad::Up => (1, 0),
            Pad::UpRight => (2, 0),
            Pad::Left => (0, 1),
            Pad::Right => (2, 1),
            Pad::DownLeft => (0, 2),
            Pad::Down => (1, 2),
            Pad::DownRight => (2, 2),
        }
    }
}

/// Every stop the API has, for the Stop button.
pub fn all_stops() -> (Vec<PtzDirection>, Vec<ZoomControl>) {
    (
        vec![
            PtzDirection::UpStop,
            PtzDirection::DownStop,
            PtzDirection::LeftStop,
            PtzDirection::RightStop,
        ],
        vec![ZoomControl::InStop, ZoomControl::OutStop],
    )
}

/// Which camera the pad drives right now (the slot's current camera).
pub type CameraSource = Rc<dyn Fn() -> Option<Camera>>;

struct Ctx {
    store: Rc<Store>,
    services: Rc<Services>,
    camera: CameraSource,
    slot: usize,
}

impl Ctx {
    fn camera_id(&self) -> Option<i64> {
        (self.camera)().map(|c| c.id)
    }

    /// Run one PTZ call on the runtime; failures become a toast.
    fn call<F, Fut>(&self, what: &'static str, f: F)
    where
        F: FnOnce(Client, i64) -> Fut,
        Fut: Future<Output = ss_api::Result<()>> + Send + 'static,
    {
        let Some(id) = self.camera_id() else { return };
        let Some(client) = self.services.client() else {
            self.store.toast("Not connected");
            return;
        };
        tracing::debug!(camera = id, "PTZ {what}");
        let fut = self.services.run(f(client, id));
        let store = self.store.clone();
        glib::spawn_future_local(async move {
            if let Err(e) = fut.await {
                tracing::warn!("PTZ {what}: {e}");
                store.toast(format!("PTZ failed: {e}"));
            }
        });
    }

    fn moves(&self, dirs: Vec<PtzDirection>) {
        for d in dirs {
            self.call(
                d.as_str(),
                move |c, id| async move { c.ptz_move(id, d).await },
            );
        }
    }

    fn zoom(&self, z: ZoomControl) {
        self.call(
            z.as_str(),
            move |c, id| async move { c.ptz_zoom(id, z).await },
        );
    }

    fn focus(&self, f: FocusControl) {
        let what = match f {
            FocusControl::InStart => "focus in start",
            FocusControl::InStop => "focus in stop",
            FocusControl::OutStart => "focus out start",
            FocusControl::OutStop => "focus out stop",
        };
        self.call(what, move |c, id| async move { c.ptz_focus(id, f).await });
    }

    fn name(&self, suffix: &str) -> String {
        format!("slot-{}-ptz-{suffix}", self.slot)
    }
}

/// A button that fires `start` on press and `stop` on release (or nudges
/// on a bare `clicked`).
fn hold_button(
    material: &str,
    name: &str,
    tooltip: &str,
    start: Rc<dyn Fn()>,
    stop: Rc<dyn Fn()>,
) -> gtk::Button {
    let b = icons::flat_button(material, "icon-32", tooltip);
    b.add_css_class("ptz-key");
    b.set_widget_name(name);
    b.set_focus_on_click(false);
    let held = Rc::new(Cell::new(false));
    let last_change = Rc::new(Cell::new(None::<Instant>));
    {
        let held = held.clone();
        let last_change = last_change.clone();
        let (start, stop) = (start.clone(), stop.clone());
        b.connect_state_flags_changed(move |b, old| {
            let active = gtk::StateFlags::ACTIVE;
            let now = b.state_flags().contains(active);
            let was = old.contains(active);
            if now && !was {
                held.set(true);
                last_change.set(Some(Instant::now()));
                start();
            } else if was && !now && held.replace(false) {
                last_change.set(Some(Instant::now()));
                stop();
            }
        });
    }
    b.connect_clicked(move |_| {
        let recent = last_change
            .get()
            .is_some_and(|t| t.elapsed() < Duration::from_millis(100));
        if held.get() || recent {
            return; // the press/release path already moved the camera
        }
        start();
        let stop = stop.clone();
        glib::timeout_add_local_once(NUDGE, move || stop());
    });
    b
}

/// The pad's widgets plus the hook that reloads title and lists for the
/// slot's current camera (run each time the popover opens).
pub struct PtzPad {
    pub root: gtk::Box,
    pub refresh: Rc<dyn Fn()>,
}

/// Build the popover for one slot. The contents are rebuilt for the
/// slot's current camera each time it opens.
pub fn popover(
    store: &Rc<Store>,
    services: &Rc<Services>,
    slot: usize,
    camera: CameraSource,
) -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.add_css_class("ptz-pop");
    popover.set_widget_name(&format!("slot-{slot}-ptz-popover"));
    popover.set_has_arrow(false);
    popover.set_position(gtk::PositionType::Top);
    let pad = build(store, services, slot, camera, Some(popover.downgrade()));
    popover.set_child(Some(&pad.root));
    let refresh = pad.refresh.clone();
    popover.connect_show(move |_| refresh());
    popover
}

/// Build the pad itself. `popover` (if any) is closed after a preset or
/// patrol is chosen. Used by [`popover`] and by the headless driver, which
/// shows the pad in a plain window because Broadway cannot map popups.
pub fn build(
    store: &Rc<Store>,
    services: &Rc<Services>,
    slot: usize,
    camera: CameraSource,
    popover: Option<glib::WeakRef<gtk::Popover>>,
) -> PtzPad {
    let ctx = Rc::new(Ctx {
        store: store.clone(),
        services: services.clone(),
        camera,
        slot,
    });

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.add_css_class("ptz-pad-root");
    content.set_width_request(200);

    let title = label("PTZ", &["ptz-title"]);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_xalign(0.0);
    content.append(&title);

    // --- direction pad
    let pad = gtk::Grid::new();
    pad.add_css_class("ptz-pad");
    pad.set_row_spacing(2);
    pad.set_column_spacing(2);
    pad.set_halign(gtk::Align::Center);
    for dir in Pad::ALL {
        let (icon, suffix) = dir.icon();
        let (starts, stops) = dir.moves();
        let start: Rc<dyn Fn()> = {
            let ctx = ctx.clone();
            Rc::new(move || ctx.moves(starts.clone()))
        };
        let stop: Rc<dyn Fn()> = {
            let ctx = ctx.clone();
            Rc::new(move || ctx.moves(stops.clone()))
        };
        let b = hold_button(
            icon,
            &ctx.name(suffix),
            &format!("Move {suffix}"),
            start,
            stop,
        );
        let (col, row) = dir.cell();
        pad.attach(&b, col, row, 1, 1);
    }
    let home = icons::flat_button("home", "icon-32", "Home position");
    home.add_css_class("ptz-key");
    home.set_widget_name(&ctx.name("home"));
    {
        let ctx = ctx.clone();
        home.connect_clicked(move |_| ctx.moves(vec![PtzDirection::Home]));
    }
    pad.attach(&home, 1, 1, 1, 1);
    content.append(&pad);

    // --- zoom / focus / stop
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    row.set_halign(gtk::Align::Center);
    let zoom_pair = |z_in: ZoomControl, z_out: ZoomControl, icon: &str, suffix: &str, tip: &str| {
        let start: Rc<dyn Fn()> = {
            let ctx = ctx.clone();
            Rc::new(move || ctx.zoom(z_in))
        };
        let stop: Rc<dyn Fn()> = {
            let ctx = ctx.clone();
            Rc::new(move || ctx.zoom(z_out))
        };
        hold_button(icon, &ctx.name(suffix), tip, start, stop)
    };
    row.append(&zoom_pair(
        ZoomControl::OutStart,
        ZoomControl::OutStop,
        "zoom_out",
        "zoom-out",
        "Zoom out (hold)",
    ));
    row.append(&zoom_pair(
        ZoomControl::InStart,
        ZoomControl::InStop,
        "zoom_in",
        "zoom-in",
        "Zoom in (hold)",
    ));
    let focus_pair =
        |f_start: FocusControl, f_stop: FocusControl, icon: &str, suffix: &str, tip: &str| {
            let start: Rc<dyn Fn()> = {
                let ctx = ctx.clone();
                Rc::new(move || ctx.focus(f_start))
            };
            let stop: Rc<dyn Fn()> = {
                let ctx = ctx.clone();
                Rc::new(move || ctx.focus(f_stop))
            };
            hold_button(icon, &ctx.name(suffix), tip, start, stop)
        };
    row.append(&focus_pair(
        FocusControl::InStart,
        FocusControl::InStop,
        "center_focus_strong",
        "focus-near",
        "Focus near (hold)",
    ));
    row.append(&focus_pair(
        FocusControl::OutStart,
        FocusControl::OutStop,
        "center_focus_weak",
        "focus-far",
        "Focus far (hold)",
    ));
    let stop = icons::flat_button("stop_circle", "icon-32", "Stop all movement");
    stop.add_css_class("ptz-key");
    stop.add_css_class("danger");
    stop.set_widget_name(&ctx.name("stop"));
    {
        let ctx = ctx.clone();
        stop.connect_clicked(move |_| {
            let (dirs, zooms) = all_stops();
            ctx.moves(dirs);
            for z in zooms {
                ctx.zoom(z);
            }
        });
    }
    row.append(&stop);
    content.append(&row);

    // --- presets / patrols (filled on show)
    content.append(&divider_h());
    let lists = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.append(&lists);

    let generation = Rc::new(Cell::new(0u64));
    let loading: Rc<RefCell<Option<glib::JoinHandle<()>>>> = Rc::new(RefCell::new(None));
    let refresh: Rc<dyn Fn()> = {
        let ctx = ctx.clone();
        let title = title.clone();
        let lists = lists.clone();
        let popover_w = popover.clone();
        let content_w = content.downgrade();
        Rc::new(move || {
            let camera = (ctx.camera)();
            title.set_text(&match &camera {
                Some(c) => format!("PTZ · {}", c.name),
                None => "PTZ".to_string(),
            });
            clear(&lists);
            let Some(camera) = camera else { return };
            let Some(client) = ctx.services.client() else {
                lists.append(&label("Not connected", &["t12", "muted"]));
                return;
            };
            lists.append(&label("Loading presets…", &["t12", "muted"]));
            generation.set(generation.get() + 1);
            let my_gen = generation.get();
            let generation = generation.clone();
            let id = camera.id;
            let fut = ctx.services.run(async move {
                let presets = client.ptz_presets(id).await;
                let patrols = client.ptz_patrols(id).await;
                (presets, patrols)
            });
            let ctx = ctx.clone();
            let lists = lists.clone();
            let popover_w = popover_w.clone();
            let content_w = content_w.clone();
            let handle = glib::spawn_future_local(async move {
                let (presets, patrols) = fut.await;
                if generation.get() != my_gen || content_w.upgrade().is_none() {
                    return;
                }
                clear(&lists);
                fill_lists(&ctx, &lists, popover_w.as_ref(), presets, patrols);
            });
            if let Some(old) = loading.borrow_mut().replace(handle) {
                old.abort();
            }
        })
    };
    PtzPad {
        root: content,
        refresh,
    }
}

fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

fn fill_lists(
    ctx: &Rc<Ctx>,
    lists: &gtk::Box,
    popover: Option<&glib::WeakRef<gtk::Popover>>,
    presets: ss_api::Result<Vec<ss_api::PtzPreset>>,
    patrols: ss_api::Result<Vec<ss_api::PtzPatrol>>,
) {
    let section = |text: &str| {
        let l = label(text, &["ptz-section"]);
        l.set_xalign(0.0);
        l
    };
    match presets {
        Ok(presets) if presets.is_empty() => {
            lists.append(&label("No presets", &["t12", "muted"]));
        }
        Ok(presets) => {
            lists.append(&section("PRESETS"));
            for p in presets {
                let row = menu_row("location_on", &p.name, None);
                row.set_widget_name(&ctx.name(&format!("preset-{}", p.id)));
                let ctx = ctx.clone();
                let popover = popover.cloned();
                row.connect_clicked(move |_| {
                    let (pid, name) = (p.id, p.name.clone());
                    ctx.call("go preset", move |c, id| async move {
                        c.ptz_go_preset(id, pid).await
                    });
                    ctx.store.toast(format!("Preset · {name}"));
                    if let Some(p) = popover.as_ref().and_then(|w| w.upgrade()) {
                        p.popdown();
                    }
                });
                lists.append(&row);
            }
        }
        Err(e) => {
            tracing::debug!("PTZ presets: {e}");
            lists.append(&label("Presets unavailable", &["t12", "muted"]));
        }
    }
    if let Ok(patrols) = patrols
        && !patrols.is_empty()
    {
        lists.append(&section("PATROLS"));
        for p in patrols {
            let row = menu_row("route", &p.name, None);
            row.set_widget_name(&ctx.name(&format!("patrol-{}", p.id)));
            let ctx = ctx.clone();
            let popover = popover.cloned();
            row.connect_clicked(move |_| {
                let (pid, name) = (p.id, p.name.clone());
                ctx.call("run patrol", move |c, id| async move {
                    c.ptz_run_patrol(id, pid).await
                });
                ctx.store.toast(format!("Patrol · {name}"));
                if let Some(p) = popover.as_ref().and_then(|w| w.upgrade()) {
                    p.popdown();
                }
            });
            lists.append(&row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagonals_are_two_moves_and_stop_everything_they_start() {
        for dir in Pad::ALL {
            let (starts, stops) = dir.moves();
            assert_eq!(starts.len(), stops.len());
            let diagonal = matches!(
                dir,
                Pad::UpLeft | Pad::UpRight | Pad::DownLeft | Pad::DownRight
            );
            assert_eq!(starts.len(), if diagonal { 2 } else { 1 });
            for (s, e) in starts.iter().zip(&stops) {
                assert!(s.as_str().ends_with("Start"));
                assert!(e.as_str().ends_with("Stop"));
                assert_eq!(
                    s.as_str().trim_end_matches("Start"),
                    e.as_str().trim_end_matches("Stop")
                );
            }
        }
    }

    #[test]
    fn pad_cells_are_distinct_and_leave_the_centre() {
        let mut cells: Vec<(i32, i32)> = Pad::ALL.iter().map(|d| d.cell()).collect();
        cells.sort();
        cells.dedup();
        assert_eq!(cells.len(), 8);
        assert!(!cells.contains(&(1, 1)));
    }

    #[test]
    fn stop_covers_every_axis() {
        let (dirs, zooms) = all_stops();
        assert_eq!(dirs.len(), 4);
        assert_eq!(zooms.len(), 2);
        assert!(dirs.iter().all(|d| d.as_str().ends_with("Stop")));
    }
}
