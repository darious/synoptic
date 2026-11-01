//! The Live View grid: slots on black with 1 px gutters laid out by the
//! active layout's [`Shape`] (spanning cells for `1+n`), selection, focus
//! (one slot shown alone, the others hidden but still streaming so the
//! layout comes back instantly) and the playback controller the timeline
//! drives (pause / speed / seek / go live).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use ss_api::Camera;
use ss_stream::Speed;

use super::slot::{Slot, Status};
use crate::services::Services;
use crate::state::{Change, Shape, Store};

/// Speed dropdown entries (label, multiplier), ¼× … 16×.
pub const SPEEDS: [(&str, f64); 7] = [
    ("¼×", 0.25),
    ("½×", 0.5),
    ("1×", 1.0),
    ("2×", 2.0),
    ("4×", 4.0),
    ("8×", 8.0),
    ("16×", 16.0),
];

/// Index of 1× in [`SPEEDS`].
pub const SPEED_NORMAL: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Live,
    History,
}

/// Snapshot of the playback state for the timeline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Playback {
    pub mode: Mode,
    /// History: wall-clock position of the tracked slot (unix ms), if known.
    pub position_ms: Option<i64>,
    pub paused: bool,
    pub speed_index: usize,
}

pub struct Grid {
    widget: gtk::Grid,
    store: Rc<Store>,
    services: Rc<Services>,
    slots: RefCell<Vec<Rc<Slot>>>,
    shape: Cell<Option<Shape>>,
    paused: Cell<bool>,
    speed_index: Cell<usize>,
    /// Focus as last applied, to notice the return to the layout.
    last_focus: Cell<Option<usize>>,
    listeners: RefCell<Vec<Rc<dyn Fn()>>>,
}

impl Grid {
    pub fn new(store: &Rc<Store>, services: &Rc<Services>) -> Rc<Grid> {
        let widget = gtk::Grid::new();
        widget.add_css_class("live-grid");
        widget.set_row_homogeneous(true);
        widget.set_column_homogeneous(true);
        widget.set_row_spacing(1);
        widget.set_column_spacing(1);
        widget.set_hexpand(true);
        widget.set_vexpand(true);

        let grid = Rc::new(Grid {
            widget,
            store: store.clone(),
            services: services.clone(),
            slots: RefCell::new(Vec::new()),
            shape: Cell::new(None),
            paused: Cell::new(false),
            speed_index: Cell::new(SPEED_NORMAL),
            last_focus: Cell::new(None),
            listeners: RefCell::new(Vec::new()),
        });
        grid.sync();

        {
            let w = Rc::downgrade(&grid);
            store.subscribe(move |change| {
                let Some(grid) = w.upgrade() else { return };
                match change {
                    Change::Layout | Change::Slots | Change::Cameras | Change::Connection => {
                        grid.sync();
                    }
                    Change::SelectedSlot => grid.sync_selection(),
                    Change::FocusedSlot => grid.sync_focus(),
                    Change::RequestReload => {
                        for slot in grid.slots.borrow().iter() {
                            slot.restart();
                        }
                    }
                    _ => {}
                }
            });
        }

        // timecodes
        {
            let w = Rc::downgrade(&grid);
            glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
                let Some(grid) = w.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let now = chrono::Local::now();
                for slot in grid.slots.borrow().iter() {
                    slot.tick(now);
                }
                glib::ControlFlow::Continue
            });
        }

        grid
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.widget.upcast_ref()
    }

    /// Called when mode/pause/speed change (not on every position update).
    pub fn subscribe(&self, f: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Rc::new(f));
    }

    fn notify(&self) {
        // clone the list so a listener may subscribe re-entrantly
        let listeners: Vec<Rc<dyn Fn()>> = self.listeners.borrow().clone();
        for l in listeners {
            l();
        }
    }

    // --- state the timeline reads --------------------------------------------

    /// The slot the timeline tracks: the focused one, else the selected
    /// one, else the first filled.
    pub fn tracked_slot(&self) -> Option<Rc<Slot>> {
        let slots = self.slots.borrow();
        let (focused, selected) = {
            let s = self.store.get();
            (s.focused_slot, s.selected_slot)
        };
        focused
            .or(selected)
            .and_then(|i| slots.get(i).cloned())
            .filter(|s| s.camera_id().is_some())
            .or_else(|| slots.iter().find(|s| s.camera_id().is_some()).cloned())
    }

    /// Slots currently shown: all of them, or just the focused one.
    fn visible_slots(&self) -> Vec<Rc<Slot>> {
        let focused = self.store.get().focused_slot;
        self.slots
            .borrow()
            .iter()
            .filter(|s| focused.is_none_or(|f| f == s.index()))
            .cloned()
            .collect()
    }

    pub fn tracked_camera(&self) -> Option<Camera> {
        self.tracked_slot().and_then(|s| s.camera())
    }

    /// Cameras on screen (timeline lanes): the layout's, or only the
    /// focused slot's.
    pub fn camera_ids(&self) -> Vec<i64> {
        self.visible_slots()
            .iter()
            .filter_map(|s| s.camera_id())
            .collect()
    }

    pub fn playback(&self) -> Playback {
        let tracked = self.tracked_slot();
        let history = tracked.as_ref().is_some_and(|s| s.is_history())
            || self
                .slots
                .borrow()
                .iter()
                .any(|s| s.is_history() && s.status() != Status::Empty);
        Playback {
            mode: if history { Mode::History } else { Mode::Live },
            position_ms: tracked.and_then(|s| s.position_ms()),
            paused: self.paused.get(),
            speed_index: self.speed_index.get(),
        }
    }

    pub fn speed(&self) -> Speed {
        Speed::new(SPEEDS[self.speed_index.get().min(SPEEDS.len() - 1)].1).unwrap_or_default()
    }

    // --- playback control ----------------------------------------------------

    /// Slots a seek applies to: every filled slot. The grid shares one
    /// timeline, so every camera shows the same moment.
    fn targets(&self) -> Vec<Rc<Slot>> {
        self.slots
            .borrow()
            .iter()
            .filter(|s| s.camera_id().is_some())
            .cloned()
            .collect()
    }

    /// Seek every filled slot to wall-clock `target_secs` → History mode.
    pub fn seek(&self, target_secs: i64) {
        let speed = self.speed();
        let paused = self.paused.get();
        for slot in self.targets() {
            slot.seek(target_secs, speed, paused);
        }
        self.notify();
    }

    /// Back to live on every slot; resets pause and speed.
    pub fn go_live(&self) {
        self.paused.set(false);
        self.speed_index.set(SPEED_NORMAL);
        for slot in self.slots.borrow().iter() {
            slot.go_live();
        }
        self.notify();
    }

    pub fn set_paused(&self, paused: bool) {
        if self.paused.replace(paused) == paused {
            return;
        }
        for slot in self.slots.borrow().iter() {
            slot.set_paused(paused);
        }
        self.notify();
    }

    pub fn set_speed_index(&self, index: usize) {
        let index = index.min(SPEEDS.len() - 1);
        if self.speed_index.replace(index) == index {
            return;
        }
        let speed = self.speed();
        for slot in self.slots.borrow().iter() {
            slot.set_speed(speed);
        }
        self.notify();
    }

    // --- store → widgets -----------------------------------------------------

    fn sync(&self) {
        let (shape, slot_ids, cameras, selected, logged_in) = {
            let s = self.store.get();
            (
                s.shape(),
                s.slots(),
                s.cameras.clone(),
                s.selected_slot,
                s.logged_in(),
            )
        };

        if self.shape.get() != Some(shape) {
            self.rebuild_slots(shape);
            self.shape.set(Some(shape));
        }

        let slots = self.slots.borrow();
        for (slot, &id) in slots.iter().zip(slot_ids.iter()) {
            let camera = if logged_in && id != 0 {
                cameras.iter().find(|c| c.id == id).cloned()
            } else {
                None
            };
            slot.set_camera(camera);
            slot.set_selected(selected == Some(slot.index()));
        }
        drop(slots);
        self.apply_focus();
        self.notify();
    }

    fn sync_focus(&self) {
        self.apply_focus();
        self.notify();
    }

    /// Show only the focused slot (GTK's homogeneous grid collapses rows and
    /// columns with no visible child, so it fills the stage). Focus on an
    /// empty or missing slot is dropped.
    fn apply_focus(&self) {
        let focused = self.store.get().focused_slot;
        let slots = self.slots.borrow();
        let valid = focused.is_some_and(|f| slots.get(f).is_some_and(|s| s.camera_id().is_some()));
        if focused.is_some() && !valid {
            drop(slots);
            self.store
                .update(Change::FocusedSlot, |s| s.focused_slot = None);
            return;
        }
        for slot in slots.iter() {
            let show = focused.is_none_or(|f| f == slot.index());
            slot.widget().set_visible(show);
            slot.set_focused(show && focused.is_some());
            // hidden slots stop decoding; their stream stays connected
            slot.set_suspended(!show);
        }
        // back to the layout in history mode: the hidden slots were frozen
        // while the focused one played on, so line them up again
        let returned = self.last_focus.replace(focused).is_some() && focused.is_none();
        drop(slots);
        if returned
            && self.playback().mode == Mode::History
            && let Some(ms) = self.tracked_slot().and_then(|s| s.position_ms())
        {
            self.seek(ms / 1000);
        }
    }

    fn sync_selection(&self) {
        let selected = self.store.get().selected_slot;
        for slot in self.slots.borrow().iter() {
            slot.set_selected(selected == Some(slot.index()));
        }
        self.notify();
    }

    fn rebuild_slots(&self, shape: Shape) {
        let old = std::mem::take(&mut *self.slots.borrow_mut());
        for slot in &old {
            self.widget.remove(slot.widget());
        }
        drop(old);
        let mut slots = Vec::with_capacity(shape.slots());
        for i in 0..shape.slots() {
            let slot = Slot::new(i, &self.store, &self.services);
            let cell = shape.cell(i);
            self.widget.attach(
                slot.widget(),
                cell.col,
                cell.row,
                cell.colspan,
                cell.rowspan,
            );
            slots.push(slot);
        }
        *self.slots.borrow_mut() = slots;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speeds_are_sorted_and_contain_normal() {
        assert_eq!(SPEEDS[SPEED_NORMAL].1, 1.0);
        assert!(SPEEDS.windows(2).all(|w| w[0].1 < w[1].1));
        assert_eq!(SPEEDS.first().map(|s| s.1), Some(0.25));
        assert_eq!(SPEEDS.last().map(|s| s.1), Some(16.0));
        for (_, m) in SPEEDS {
            assert!(Speed::new(m).is_some());
        }
    }
}
