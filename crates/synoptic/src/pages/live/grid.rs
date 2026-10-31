//! The n×n Live View grid: slots on black with 1 px gutters, selection,
//! per-layout camera memory (from `AppState.slots_by_layout`), and the
//! playback controller the timeline drives (pause / speed / seek / go live).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use ss_api::Camera;
use ss_stream::Speed;

use super::slot::{Slot, Status};
use crate::services::Services;
use crate::state::{Change, Layout, Store};

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
    layout: Cell<Option<Layout>>,
    paused: Cell<bool>,
    speed_index: Cell<usize>,
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
            layout: Cell::new(None),
            paused: Cell::new(false),
            speed_index: Cell::new(SPEED_NORMAL),
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
                    Change::RequestReload => {
                        for slot in grid.slots.borrow().iter() {
                            slot.restart();
                        }
                    }
                    Change::RequestOpenSingle(id) => grid.open_single(*id),
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

    /// The slot the timeline tracks: the selected one, else the first filled.
    pub fn tracked_slot(&self) -> Option<Rc<Slot>> {
        let slots = self.slots.borrow();
        let selected = self.store.get().selected_slot;
        selected
            .and_then(|i| slots.get(i).cloned())
            .filter(|s| s.camera_id().is_some())
            .or_else(|| slots.iter().find(|s| s.camera_id().is_some()).cloned())
    }

    pub fn tracked_camera(&self) -> Option<Camera> {
        self.tracked_slot().and_then(|s| s.camera())
    }

    /// Cameras in the visible layout (for presence row B).
    pub fn camera_ids(&self) -> Vec<i64> {
        self.slots
            .borrow()
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

    /// Slots a seek applies to: the selected one, else every filled slot.
    fn targets(&self) -> Vec<Rc<Slot>> {
        let slots = self.slots.borrow();
        match self
            .store
            .get()
            .selected_slot
            .and_then(|i| slots.get(i).cloned())
            .filter(|s| s.camera_id().is_some())
        {
            Some(s) => vec![s],
            None => slots
                .iter()
                .filter(|s| s.camera_id().is_some())
                .cloned()
                .collect(),
        }
    }

    /// Seek to wall-clock `target_secs` → History mode.
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
        let (layout, slot_ids, cameras, selected, logged_in) = {
            let s = self.store.get();
            (
                s.layout,
                s.slots(),
                s.cameras.clone(),
                s.selected_slot,
                s.logged_in(),
            )
        };

        if self.layout.get() != Some(layout) {
            self.rebuild_slots(layout);
            self.layout.set(Some(layout));
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
        self.notify();
    }

    fn sync_selection(&self) {
        let selected = self.store.get().selected_slot;
        for slot in self.slots.borrow().iter() {
            slot.set_selected(selected == Some(slot.index()));
        }
        self.notify();
    }

    fn rebuild_slots(&self, layout: Layout) {
        let old = std::mem::take(&mut *self.slots.borrow_mut());
        for slot in &old {
            self.widget.remove(slot.widget());
        }
        drop(old);
        let n = layout.n();
        let mut slots = Vec::with_capacity(layout.slots());
        for i in 0..layout.slots() {
            let slot = Slot::new(i, &self.store, &self.services);
            let (col, row) = cell_for(i, n);
            self.widget.attach(slot.widget(), col, row, 1, 1);
            slots.push(slot);
        }
        *self.slots.borrow_mut() = slots;
    }

    /// `RequestOpenSingle`: 1×1 with this camera.
    fn open_single(&self, camera_id: i64) {
        self.store.update(Change::Slots, |s| {
            s.slots_by_layout.insert(Layout::L1, vec![camera_id]);
            s.selected_slot = None;
        });
        self.store.update(Change::Layout, |s| s.layout = Layout::L1);
        self.store.emit(&Change::SelectedSlot);
    }
}

/// Grid cell (column, row) of slot `index` in an `n`×`n` layout, row-major.
pub fn cell_for(index: usize, n: usize) -> (i32, i32) {
    let n = n.max(1);
    ((index % n) as i32, (index / n) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_are_row_major() {
        assert_eq!(cell_for(0, 1), (0, 0));
        assert_eq!(cell_for(0, 2), (0, 0));
        assert_eq!(cell_for(1, 2), (1, 0));
        assert_eq!(cell_for(2, 2), (0, 1));
        assert_eq!(cell_for(3, 2), (1, 1));
        assert_eq!(cell_for(5, 3), (2, 1));
        assert_eq!(cell_for(15, 4), (3, 3));
    }

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
