//! One Live View slot: a `gtk::Picture` under overlays (name pill, status
//! pill, timecode, hover toolbar) plus the empty and offline states, and the
//! [`VideoStream`] that feeds it. See the UI spec §1 "Slot".

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use ss_api::{Camera, CameraStatus};
use ss_stream::Speed;

use crate::icons;
use crate::pages::live::ptz;
use crate::services::Services;
use crate::state::{Change, Store};
use crate::video::bridge::{
    self, HistoryRecording, StreamAuth, StreamEvent, StreamSource, VideoStream,
};
use crate::video::pipeline::Codec;
use crate::widgets::controls::{divider_v, label, set_class};

/// What the slot shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Empty,
    /// Camera assigned, stream not yet delivering.
    Connecting,
    Live,
    History,
    Offline,
}

pub struct Slot {
    index: usize,
    store: Rc<Store>,
    services: Rc<Services>,
    root: gtk::Overlay,
    picture: gtk::Picture,
    name_pill: gtk::Box,
    name_label: gtk::Label,
    model_label: gtk::Label,
    status_pill: gtk::Box,
    status_label: gtk::Label,
    timecode: gtk::Label,
    toolbar: gtk::Box,
    ptz_group: gtk::Box,
    mute_icon: gtk::Image,
    focus_button: gtk::Button,
    empty_box: gtk::Box,
    empty_hint: gtk::Label,
    offline_label: gtk::Label,

    camera: RefCell<Option<Camera>>,
    status: Cell<Status>,
    selected: Cell<bool>,
    /// Shown alone (double-click / "Fill view").
    focused: Cell<bool>,
    hovered: Cell<bool>,
    muted: Cell<bool>,
    stream: RefCell<Option<VideoStream>>,
    /// History: last wall-clock position reported (unix ms).
    position_ms: Cell<Option<i64>>,
    /// History: when the last video frame arrived (end-of-file watchdog).
    last_frame_at: Cell<Option<std::time::Instant>>,
    /// Pause state the grid asked for (history frames stop while paused).
    paused: Cell<bool>,
    /// A "next recording" lookup is in flight; don't start another.
    advancing: Cell<bool>,
    /// Bumped whenever the stream is (re)started so stale async results are ignored.
    generation: Cell<u64>,
    retry: RefCell<Option<glib::SourceId>>,
}

const FOCUS_TOOLTIP: &str = "Fill the view with this camera (double-click)";
const UNFOCUS_TOOLTIP: &str = "Back to the layout (double-click)";

/// History: no frame for this long while the position sits at the end of
/// the file counts as "the recording ended" even without a close notice.
const END_OF_FILE_SILENCE: std::time::Duration = std::time::Duration::from_millis(2500);
/// History: a recording that stops within this many seconds of now is the
/// one still being written; past its end the slot goes back to live.
const LIVE_EDGE_SLACK_SECS: i64 = 120;

impl Slot {
    pub fn new(index: usize, store: &Rc<Store>, services: &Rc<Services>) -> Rc<Slot> {
        let root = gtk::Overlay::new();
        root.add_css_class("slot");
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_overflow(gtk::Overflow::Hidden);

        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        picture.set_can_shrink(true);
        root.set_child(Some(&picture));

        // top-left name pill
        let name_pill = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        name_pill.add_css_class("pill");
        name_pill.set_halign(gtk::Align::Start);
        name_pill.set_valign(gtk::Align::Start);
        name_pill.set_margin_start(8);
        name_pill.set_margin_top(8);
        let name_label = label("", &["name"]);
        name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let model_label = label("", &["model"]);
        model_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        name_pill.append(&name_label);
        name_pill.append(&model_label);
        root.add_overlay(&name_pill);

        // top-right status pill
        let status_pill = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        status_pill.add_css_class("status-pill");
        status_pill.set_halign(gtk::Align::End);
        status_pill.set_valign(gtk::Align::Start);
        status_pill.set_margin_end(8);
        status_pill.set_margin_top(8);
        let status_dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        status_dot.add_css_class("dot");
        status_dot.set_valign(gtk::Align::Center);
        let status_label = gtk::Label::new(Some("LIVE"));
        status_pill.append(&status_dot);
        status_pill.append(&status_label);
        root.add_overlay(&status_pill);

        // bottom-right timecode
        let timecode = gtk::Label::new(Some("--:--:--"));
        timecode.add_css_class("timecode");
        timecode.set_halign(gtk::Align::End);
        timecode.set_valign(gtk::Align::End);
        timecode.set_margin_end(8);
        timecode.set_margin_bottom(8);
        root.add_overlay(&timecode);

        // bottom-left hover toolbar
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        toolbar.add_css_class("slot-toolbar");
        toolbar.set_halign(gtk::Align::Start);
        toolbar.set_valign(gtk::Align::End);
        toolbar.set_margin_start(8);
        toolbar.set_margin_bottom(8);
        toolbar.set_visible(false);
        toolbar.set_widget_name(&format!("slot-{index}-toolbar"));
        // slots start muted; `load_mute` applies the per-camera config
        let mute_button = gtk::Button::new();
        let mute_icon = icons::image("volume_off", 18);
        mute_button.set_child(Some(&mute_icon));
        mute_button.set_tooltip_text(Some("Unmute"));
        mute_button.set_widget_name(&format!("slot-{index}-mute"));
        let snapshot_button = icons::flat_button("photo_camera", "icon-30", "Snapshot");
        snapshot_button.set_widget_name(&format!("slot-{index}-snapshot"));
        // PTZ cameras only (see `relabel`): one popover holds pad, zoom,
        // focus and presets
        let ptz_group = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        ptz_group.append(&divider_v());
        let ptz_button = gtk::MenuButton::new();
        ptz_button.set_child(Some(&icons::image("control_camera", 18)));
        ptz_button.add_css_class("slot-ptz");
        ptz_button.set_tooltip_text(Some("PTZ: pan, tilt, zoom, presets"));
        ptz_button.set_widget_name(&format!("slot-{index}-ptz"));
        ptz_button.set_valign(gtk::Align::Center);
        ptz_group.append(&ptz_button);
        let focus_button = icons::flat_button("open_in_full", "icon-30", FOCUS_TOOLTIP);
        focus_button.set_widget_name(&format!("slot-{index}-focus"));
        toolbar.append(&mute_button);
        toolbar.append(&snapshot_button);
        toolbar.append(&ptz_group);
        toolbar.append(&divider_v());
        toolbar.append(&focus_button);
        root.add_overlay(&toolbar);

        // empty state
        let empty_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        empty_box.set_halign(gtk::Align::Center);
        empty_box.set_valign(gtk::Align::Center);
        let empty_icon = icons::image("add_a_photo", 28);
        empty_icon.add_css_class("slot-empty-icon");
        let empty_hint = gtk::Label::new(Some("Click, then pick a camera"));
        empty_hint.add_css_class("slot-empty-hint");
        empty_box.append(&empty_icon);
        empty_box.append(&empty_hint);
        root.add_overlay(&empty_box);

        // offline placeholder text
        let offline_label = gtk::Label::new(Some("camera offline · reconnecting"));
        offline_label.add_css_class("slot-offline-text");
        offline_label.set_halign(gtk::Align::Center);
        offline_label.set_valign(gtk::Align::Center);
        offline_label.set_visible(false);
        root.add_overlay(&offline_label);

        let slot = Rc::new(Slot {
            index,
            store: store.clone(),
            services: services.clone(),
            root,
            picture,
            name_pill,
            name_label,
            model_label,
            status_pill,
            status_label,
            timecode,
            toolbar,
            ptz_group,
            mute_icon,
            focus_button: focus_button.clone(),
            empty_box,
            empty_hint,
            offline_label,
            camera: RefCell::new(None),
            status: Cell::new(Status::Empty),
            selected: Cell::new(false),
            focused: Cell::new(false),
            hovered: Cell::new(false),
            muted: Cell::new(true),
            stream: RefCell::new(None),
            position_ms: Cell::new(None),
            last_frame_at: Cell::new(None),
            paused: Cell::new(false),
            advancing: Cell::new(false),
            generation: Cell::new(0),
            retry: RefCell::new(None),
        });

        // hover
        let motion = gtk::EventControllerMotion::new();
        {
            let w = Rc::downgrade(&slot);
            motion.connect_enter(move |_, _, _| {
                if let Some(s) = w.upgrade() {
                    s.hovered.set(true);
                    s.refresh();
                }
            });
        }
        {
            let w = Rc::downgrade(&slot);
            motion.connect_leave(move |_| {
                if let Some(s) = w.upgrade() {
                    s.hovered.set(false);
                    s.refresh();
                }
            });
        }
        slot.root.add_controller(motion);

        // click selects (click again deselects); double-click fills the
        // view with this slot, or returns to the layout when it already does
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        {
            let w = Rc::downgrade(&slot);
            click.connect_released(move |gesture, n_press, _, _| {
                let Some(s) = w.upgrade() else { return };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if n_press >= 2 {
                    s.toggle_focus();
                    return;
                }
                let index = s.index;
                s.store.update(Change::SelectedSlot, |st| {
                    st.selected_slot = if st.selected_slot == Some(index) {
                        None
                    } else {
                        Some(index)
                    };
                });
            });
        }
        slot.root.add_controller(click);

        // toolbar actions
        {
            let w = Rc::downgrade(&slot);
            mute_button.connect_clicked(move |_| {
                if let Some(s) = w.upgrade() {
                    s.toggle_mute();
                }
            });
        }
        {
            let w = Rc::downgrade(&slot);
            snapshot_button.connect_clicked(move |_| {
                if let Some(s) = w.upgrade() {
                    s.take_snapshot();
                }
            });
        }
        {
            let w = Rc::downgrade(&slot);
            let camera: ptz::CameraSource = Rc::new(move || w.upgrade().and_then(|s| s.camera()));
            ptz_button.set_popover(Some(&ptz::popover(store, services, index, camera)));
        }
        {
            let w = Rc::downgrade(&slot);
            focus_button.connect_clicked(move |_| {
                if let Some(s) = w.upgrade() {
                    s.toggle_focus();
                }
            });
        }

        // --- layout DnD (owned by layouts work) ---
        // Drag a filled slot (its index as a `u32`) onto another slot to swap;
        // accept a camera (`i64`, from the camera panel) or a slot (`u32`);
        // a toolbar button empties the slot.
        {
            use gtk::gdk;
            let drag = gtk::DragSource::new();
            drag.set_actions(gdk::DragAction::MOVE);
            {
                let w = Rc::downgrade(&slot);
                drag.connect_prepare(move |src, _, _| {
                    let s = w.upgrade()?;
                    s.camera_id()?;
                    src.set_icon(Some(&gtk::WidgetPaintable::new(Some(&s.name_pill))), 0, 0);
                    Some(gdk::ContentProvider::for_value(
                        &(s.index as u32).to_value(),
                    ))
                });
            }
            slot.root.add_controller(drag);

            let drop = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::MOVE);
            drop.set_types(&[i64::static_type(), u32::static_type()]);
            {
                let root = slot.root.clone();
                drop.connect_enter(move |_, _, _| {
                    root.add_css_class("drop-target");
                    gdk::DragAction::MOVE
                });
            }
            {
                let root = slot.root.clone();
                drop.connect_leave(move |_| root.remove_css_class("drop-target"));
            }
            {
                let w = Rc::downgrade(&slot);
                drop.connect_drop(move |_, value, _, _| {
                    let Some(s) = w.upgrade() else { return false };
                    s.root.remove_css_class("drop-target");
                    let index = s.index;
                    if let Ok(camera_id) = value.get::<i64>() {
                        s.store.update(Change::Slots, |st| {
                            st.place_camera(index, camera_id);
                            st.selected_slot = None;
                        });
                    } else if let Ok(from) = value.get::<u32>() {
                        let from = from as usize;
                        if from == index {
                            return true;
                        }
                        s.store.update(Change::Slots, |st| {
                            st.swap_slots(from, index);
                            st.selected_slot = None;
                        });
                    } else {
                        return false;
                    }
                    s.store.emit(&Change::SelectedSlot);
                    true
                });
            }
            slot.root.add_controller(drop);

            let remove_button = icons::flat_button("close", "icon-30", "Remove from layout");
            remove_button.set_widget_name(&format!("slot-{index}-remove"));
            {
                let w = Rc::downgrade(&slot);
                remove_button.connect_clicked(move |_| {
                    let Some(s) = w.upgrade() else { return };
                    let index = s.index;
                    s.store.update(Change::Slots, |st| {
                        st.place_camera(index, 0);
                        st.selected_slot = None;
                    });
                    s.store.emit(&Change::SelectedSlot);
                });
            }
            slot.toolbar.append(&divider_v());
            slot.toolbar.append(&remove_button);
        }
        // --- end layout DnD ---

        register(&slot);
        slot.refresh();
        slot
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn camera(&self) -> Option<Camera> {
        self.camera.borrow().clone()
    }

    pub fn camera_id(&self) -> Option<i64> {
        self.camera.borrow().as_ref().map(|c| c.id)
    }

    pub fn status(&self) -> Status {
        self.status.get()
    }

    pub fn is_history(&self) -> bool {
        self.stream
            .borrow()
            .as_ref()
            .is_some_and(|s| s.is_history())
    }

    /// History: wall-clock position (unix ms) DSM has reached.
    pub fn position_ms(&self) -> Option<i64> {
        self.position_ms.get()
    }

    pub fn set_selected(&self, selected: bool) {
        if self.selected.replace(selected) != selected {
            self.refresh();
        }
    }

    /// Grid → slot: this slot is (no longer) shown alone.
    pub fn set_focused(&self, focused: bool) {
        if self.focused.replace(focused) != focused {
            let (icon, tip) = if focused {
                ("close_fullscreen", UNFOCUS_TOOLTIP)
            } else {
                ("open_in_full", FOCUS_TOOLTIP)
            };
            self.focus_button
                .set_child(Some(&icons::image(icon, icons::icon_px("icon-30"))));
            self.focus_button.set_tooltip_text(Some(tip));
            self.refresh();
        }
    }

    /// Double-click / toolbar: fill the view with this slot, or go back to
    /// the layout. Empty slots cannot be focused.
    fn toggle_focus(&self) {
        if self.camera_id().is_none() && !self.focused.get() {
            return;
        }
        let index = self.index;
        self.store
            .update(Change::FocusedSlot, |st| st.toggle_focus(index));
        self.store.emit(&Change::SelectedSlot);
    }

    /// Assign (or clear) the camera. Restarts the stream only when the
    /// camera or its online state changed; name/model edits just relabel.
    pub fn set_camera(self: &Rc<Self>, camera: Option<Camera>) {
        let (same_id, same_online) = {
            let current = self.camera.borrow();
            (
                current.as_ref().map(|c| c.id) == camera.as_ref().map(|c| c.id),
                current.as_ref().map(|c| c.status.is_streaming())
                    == camera.as_ref().map(|c| c.status.is_streaming()),
            )
        };
        *self.camera.borrow_mut() = camera.clone();
        self.relabel();
        if !same_id {
            self.load_mute();
        }
        if same_id && same_online {
            self.refresh();
            return;
        }
        self.stop_stream();
        match camera {
            None => self.set_status(Status::Empty),
            Some(c) if c.status != CameraStatus::Enabled => self.set_status(Status::Offline),
            Some(_) => self.start_live(),
        }
    }

    /// Reload: reconnect in the current mode.
    pub fn restart(self: &Rc<Self>) {
        let source = self.stream.borrow().as_ref().map(|s| s.source());
        match source {
            Some(StreamSource::History { recording, params }) => {
                let target = self
                    .position_ms
                    .get()
                    .map(|ms| ms / 1000)
                    .unwrap_or(recording.start_time + params.start_s as i64);
                self.stop_stream();
                self.open_history(recording.clone(), target, params.speed, params.paused);
            }
            _ => {
                self.stop_stream();
                if self.camera_id().is_some() {
                    self.start_live();
                }
            }
        }
    }

    pub fn go_live(self: &Rc<Self>) {
        if self.is_history() || self.status.get() == Status::Offline {
            self.stop_stream();
            match self.camera() {
                // a disabled camera has nothing live to show; stay offline
                Some(c) if c.status != CameraStatus::Enabled => self.set_status(Status::Offline),
                Some(_) => self.start_live(),
                None => self.set_status(Status::Empty),
            }
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.set(paused);
        if let Some(s) = &*self.stream.borrow() {
            if paused {
                s.pause();
            } else {
                s.resume();
            }
        }
    }

    pub fn set_speed(&self, speed: Speed) {
        if let Some(s) = &*self.stream.borrow()
            && s.is_history()
        {
            s.set_speed(speed);
        }
    }

    /// Seek to wall-clock `target_secs` in history mode (looking the
    /// recording up when the current one does not cover it).
    pub fn seek(self: &Rc<Self>, target_secs: i64, speed: Speed, paused: bool) {
        let Some(camera_id) = self.camera_id() else {
            return;
        };
        self.advancing.set(false);
        self.paused.set(paused);
        let current = self.stream.borrow().as_ref().and_then(|s| s.recording());
        if let Some(rec) = current
            && rec.contains(target_secs)
        {
            if let Some(s) = &*self.stream.borrow() {
                s.seek_ms(rec.offset_ms(target_secs));
                if paused {
                    s.pause();
                }
            }
            self.position_ms.set(Some(target_secs * 1000));
            self.last_frame_at.set(Some(std::time::Instant::now()));
            return;
        }
        let Some(client) = self.services.client() else {
            return;
        };
        let generation = self.generation.get();
        let fut = self
            .services
            .run(bridge::find_recording(client, camera_id, target_secs));
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let found = fut.await;
            let Some(slot) = w.upgrade() else { return };
            if slot.generation.get() != generation || slot.camera_id() != Some(camera_id) {
                return;
            }
            match found {
                Some(rec) => {
                    let switched = {
                        let stream = slot.stream.borrow();
                        match stream.as_ref() {
                            Some(s) if s.is_history() => {
                                s.play(rec.clone(), rec.play_params(target_secs, speed, paused));
                                true
                            }
                            _ => false,
                        }
                    };
                    if switched {
                        slot.position_ms.set(Some(target_secs * 1000));
                        slot.last_frame_at.set(Some(std::time::Instant::now()));
                        slot.set_status(Status::Connecting);
                    } else {
                        slot.stop_stream();
                        slot.open_history(rec, target_secs, speed, paused);
                    }
                }
                None => {
                    let name = slot.camera().map(|c| c.name).unwrap_or_default();
                    slot.store
                        .toast(format!("No recording at that time · {name}"));
                }
            }
        });
    }

    /// Called ~4×/s by the grid: live timecode is the local wall clock;
    /// history shows the stream position in the timecode and the badge,
    /// and runs the end-of-file watchdog.
    pub fn tick(self: &Rc<Self>, now: chrono::DateTime<chrono::Local>) {
        match self.status.get() {
            Status::Live => {
                self.timecode.set_text(&format_timecode(&now.naive_local()));
            }
            Status::Connecting if !self.is_history() => {
                self.timecode.set_text(&format_timecode(&now.naive_local()));
            }
            Status::History | Status::Connecting => {
                if let Some(ms) = self.position_ms.get() {
                    let text = format_timecode_ms(ms);
                    self.timecode.set_text(&text);
                    if self.status.get() == Status::History {
                        self.status_label.set_text(&format!("HISTORY {text}"));
                    }
                }
                self.check_end_of_file();
            }
            _ => {}
        }
    }

    /// History: at the end of the file with no frames for a while → next.
    fn check_end_of_file(self: &Rc<Self>) {
        if self.paused.get() || self.advancing.get() {
            return;
        }
        let rec = self.stream.borrow().as_ref().and_then(|s| s.recording());
        let Some(rec) = rec else { return };
        let Some(pos) = self.position_ms.get() else {
            return;
        };
        let silent = self
            .last_frame_at
            .get()
            .is_some_and(|t| t.elapsed() >= END_OF_FILE_SILENCE);
        if silent && pos >= rec.stop_time * 1000 - 1500 {
            self.advance_after(rec);
        }
    }

    /// The current recording ended: continue into the next one (jumping
    /// over a gap to its start), back to live when the file is the one
    /// still being written, otherwise hold at the end.
    fn advance_after(self: &Rc<Self>, rec: HistoryRecording) {
        if self.advancing.replace(true) {
            return;
        }
        let Some(camera_id) = self.camera_id() else {
            return;
        };
        let Some(client) = self.services.client() else {
            return;
        };
        let now = chrono::Local::now().timestamp();
        if rec.stop_time >= now - LIVE_EDGE_SLACK_SECS {
            tracing::info!("slot {}: history caught up with live", self.index);
            self.go_live();
            return;
        }
        let generation = self.generation.get();
        let (speed, paused) = (
            self.stream
                .borrow()
                .as_ref()
                .map(|s| s.source())
                .and_then(|src| match src {
                    StreamSource::History { params, .. } => Some(params.speed),
                    _ => None,
                })
                .unwrap_or_default(),
            self.paused.get(),
        );
        let fut = self
            .services
            .run(bridge::find_next_recording(client, camera_id, rec.clone()));
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let next = fut.await;
            let Some(slot) = w.upgrade() else { return };
            if slot.generation.get() != generation || slot.camera_id() != Some(camera_id) {
                return;
            }
            match next {
                Some(next) => {
                    let gap = next.start_time - rec.stop_time;
                    tracing::info!(
                        "slot {}: recording {} ended → {} (gap {gap} s)",
                        slot.index,
                        rec.id,
                        next.id
                    );
                    let target = next.start_time;
                    let switched = {
                        let stream = slot.stream.borrow();
                        match stream.as_ref() {
                            Some(s) if s.is_history() => {
                                s.play(next.clone(), next.play_params(target, speed, paused));
                                true
                            }
                            _ => false,
                        }
                    };
                    slot.position_ms.set(Some(target * 1000));
                    slot.last_frame_at.set(Some(std::time::Instant::now()));
                    if !switched {
                        slot.stop_stream();
                        slot.open_history(next, target, speed, paused);
                    }
                    slot.advancing.set(false);
                }
                None => {
                    let name = slot.camera().map(|c| c.name).unwrap_or_default();
                    tracing::info!("slot {}: no recording after {}", slot.index, rec.id);
                    slot.store.toast(format!("End of recordings · {name}"));
                    // stay (frozen) at the end; a seek or Go live moves on
                    if let Some(s) = &*slot.stream.borrow() {
                        s.pause();
                    }
                }
            }
        });
    }

    // --- streams -----------------------------------------------------------

    fn auth(&self) -> Option<StreamAuth> {
        StreamAuth::from_session(&self.store, &self.services)
    }

    fn stop_stream(&self) {
        self.generation.set(self.generation.get() + 1);
        if let Some(id) = self.retry.borrow_mut().take() {
            id.remove();
        }
        self.stream.borrow_mut().take();
        self.position_ms.set(None);
        self.last_frame_at.set(None);
        self.advancing.set(false);
        self.picture.set_paintable(None::<&gtk::gdk::Paintable>);
    }

    fn start_live(self: &Rc<Self>) {
        let Some(camera) = self.camera() else { return };
        let Some(auth) = self.auth() else {
            self.set_status(Status::Offline);
            return;
        };
        self.set_status(Status::Connecting);
        let configured = {
            let profile_name = self
                .store
                .get()
                .connection
                .as_ref()
                .map(|c| c.profile.clone())
                .unwrap_or_default();
            bridge::configured_profile(&self.store.config(), &profile_name, camera.id)
        };
        let generation = self.generation.get();
        let fut = self.services.run(bridge::resolve_profile(
            self.services.client(),
            camera.id,
            configured,
        ));
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let profile = fut.await;
            let Some(slot) = w.upgrade() else { return };
            if slot.generation.get() != generation {
                return;
            }
            slot.open(
                &auth,
                StreamSource::Live {
                    camera_id: camera.id,
                    profile,
                },
                Codec::from_camera(camera.video_codec),
            );
        });
    }

    fn open_history(
        self: &Rc<Self>,
        recording: HistoryRecording,
        target_secs: i64,
        speed: Speed,
        paused: bool,
    ) {
        let Some(camera) = self.camera() else { return };
        let Some(auth) = self.auth() else {
            self.set_status(Status::Offline);
            return;
        };
        self.set_status(Status::Connecting);
        self.position_ms.set(Some(target_secs * 1000));
        self.last_frame_at.set(Some(std::time::Instant::now()));
        let params = recording.play_params(target_secs, speed, paused);
        self.open(
            &auth,
            StreamSource::History { recording, params },
            Codec::from_camera(camera.video_codec),
        );
    }

    fn open(self: &Rc<Self>, auth: &StreamAuth, source: StreamSource, codec: Codec) {
        let history = matches!(source, StreamSource::History { .. });
        let w = Rc::downgrade(self);
        let generation = self.generation.get();
        let opened = VideoStream::open(self.services.runtime(), auth, source, codec, move |ev| {
            let Some(slot) = w.upgrade() else { return };
            if slot.generation.get() != generation {
                return;
            }
            match ev {
                StreamEvent::Paintable(p) => slot.picture.set_paintable(Some(&p)),
                StreamEvent::Playing => {
                    slot.set_status(if history {
                        Status::History
                    } else {
                        Status::Live
                    });
                }
                StreamEvent::Reconnecting(why) => {
                    tracing::info!("slot {}: {why}", slot.index);
                    slot.set_status(Status::Offline);
                }
                StreamEvent::Offline(why) => {
                    tracing::warn!("slot {}: offline: {why}", slot.index);
                    // a history session that gave up at the end of its file
                    // is just a recording that ran out
                    let ended = slot.stream.borrow().as_ref().and_then(|s| s.recording());
                    match (ended, slot.position_ms.get()) {
                        (Some(rec), Some(pos)) if pos >= rec.stop_time * 1000 - 3000 => {
                            slot.advance_after(rec);
                        }
                        _ => {
                            slot.set_status(Status::Offline);
                            slot.schedule_retry();
                        }
                    }
                }
                StreamEvent::Position(ms) => {
                    slot.position_ms.set(Some(ms));
                    slot.last_frame_at.set(Some(std::time::Instant::now()));
                    if slot.status.get() == Status::Connecting {
                        slot.set_status(Status::History);
                    }
                }
                StreamEvent::Ended => {
                    // bind first: `advance_after` may replace the stream
                    let rec = slot.stream.borrow().as_ref().and_then(|s| s.recording());
                    if let Some(rec) = rec {
                        tracing::info!(
                            "slot {}: close notice for recording {}",
                            slot.index,
                            rec.id
                        );
                        slot.position_ms.set(Some(rec.stop_time * 1000));
                        slot.advance_after(rec);
                    }
                }
            }
        });
        match opened {
            Ok(stream) => {
                stream.set_mute(self.muted.get());
                *self.stream.borrow_mut() = Some(stream);
            }
            Err(e) => {
                tracing::warn!("stream URL: {e}");
                self.set_status(Status::Offline);
            }
        }
    }

    /// After a give-up or decoder death: try again after the camera poll interval.
    fn schedule_retry(self: &Rc<Self>) {
        let secs = self
            .store
            .config()
            .general
            .poll_interval_cameras
            .clamp(5, 300);
        let w = Rc::downgrade(self);
        let generation = self.generation.get();
        let id = glib::timeout_add_seconds_local_once(secs, move || {
            let Some(slot) = w.upgrade() else { return };
            slot.retry.borrow_mut().take();
            if slot.generation.get() != generation {
                return;
            }
            slot.restart();
        });
        if let Some(old) = self.retry.borrow_mut().replace(id) {
            old.remove();
        }
    }

    // --- actions -------------------------------------------------------------

    fn toggle_mute(self: &Rc<Self>) {
        let muted = !self.muted.get();
        self.set_muted(muted, true);
        if !muted && let Some(c) = self.camera() {
            self.store.toast(format!("Audio · {}", c.name));
        }
    }

    /// Apply a mute state to icon, pipeline and (if `persist`) the
    /// profile's `camera_muted` table. Unmuting mutes every other slot:
    /// one camera plays audio at a time, as in the Synology client.
    fn set_muted(self: &Rc<Self>, muted: bool, persist: bool) {
        self.muted.set(muted);
        self.mute_icon.set_icon_name(Some(&icons::name(if muted {
            "volume_off"
        } else {
            "volume_up"
        })));
        if let Some(button) = self.mute_icon.parent() {
            button.set_tooltip_text(Some(if muted { "Unmute" } else { "Mute" }));
        }
        if let Some(s) = &*self.stream.borrow() {
            s.set_mute(muted);
        }
        if persist && let Some(id) = self.camera_id() {
            let profile = active_profile_name(&self.store);
            let mut cfg = self.store.config();
            if let Some(p) = cfg.profiles.get_mut(&profile) {
                p.state.camera_muted.insert(id.max(0) as u32, muted);
                drop(cfg);
                self.store.save_soon();
            }
        }
        if !muted {
            for other in live_slots() {
                if other.index != self.index && !other.muted.get() {
                    other.set_muted(true, true);
                }
            }
        }
    }

    /// Mute state for the current camera from the profile's `camera_muted`
    /// table (absent = muted).
    fn load_mute(self: &Rc<Self>) {
        let muted = match self.camera_id() {
            Some(id) => {
                let profile = active_profile_name(&self.store);
                self.store
                    .config()
                    .profiles
                    .get(&profile)
                    .and_then(|p| p.state.camera_muted.get(&(id.max(0) as u32)).copied())
                    .unwrap_or(true)
            }
            None => true,
        };
        self.set_muted(muted, false);
    }

    fn take_snapshot(&self) {
        let Some(camera) = self.camera() else { return };
        let Some(client) = self.services.client() else {
            return;
        };
        let dir = std::path::PathBuf::from(self.store.config().general.snapshot_dir.clone());
        let file = dir.join(snapshot_file_name(
            &camera.name,
            &chrono::Local::now().naive_local(),
        ));
        let fut = self.services.run(async move {
            let bytes = client
                .camera_snapshot(camera.id)
                .await
                .map_err(|e| e.to_string())?;
            tokio::fs::create_dir_all(&dir)
                .await
                .map_err(|e| e.to_string())?;
            tokio::fs::write(&file, &bytes)
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(file)
        });
        let store = self.store.clone();
        let name = camera.name.clone();
        glib::spawn_future_local(async move {
            match fut.await {
                Ok(path) => {
                    tracing::info!("snapshot saved to {}", path.display());
                    store.toast(format!("Snapshot saved · {name}"));
                }
                Err(e) => {
                    tracing::warn!("snapshot: {e}");
                    store.toast(format!("Snapshot failed · {name}"));
                }
            }
        });
    }

    // --- rendering -----------------------------------------------------------

    fn set_status(&self, status: Status) {
        self.status.set(status);
        self.refresh();
    }

    fn relabel(&self) {
        let camera = self.camera.borrow();
        match camera.as_ref() {
            Some(c) => {
                self.name_label.set_text(&c.name);
                self.model_label.set_text(&c.model);
                self.model_label.set_visible(!c.model.is_empty());
                self.ptz_group.set_visible(c.is_ptz);
            }
            None => {
                self.name_label.set_text("");
                self.model_label.set_text("");
                self.ptz_group.set_visible(false);
            }
        }
    }

    fn refresh(&self) {
        let status = self.status.get();
        let has_camera = self.camera.borrow().is_some();
        set_class(&self.root, "empty", status == Status::Empty);
        set_class(&self.root, "offline", status == Status::Offline);
        set_class(&self.root, "selected", self.selected.get());
        set_class(&self.root, "focused", self.focused.get());

        self.empty_box.set_visible(status == Status::Empty);
        self.empty_hint.set_text(if self.selected.get() {
            "Pick a camera on the left"
        } else {
            "Click, then pick a camera"
        });

        self.name_pill.set_visible(has_camera);
        self.status_pill.set_visible(has_camera);
        self.timecode.set_visible(matches!(
            status,
            Status::Live | Status::History | Status::Connecting
        ));
        let disabled = self
            .camera
            .borrow()
            .as_ref()
            .is_some_and(|c| c.status != CameraStatus::Enabled);
        self.offline_label.set_text(if disabled {
            "camera disabled in Surveillance Station"
        } else {
            "camera offline · reconnecting"
        });
        self.offline_label.set_visible(status == Status::Offline);
        self.picture.set_visible(status != Status::Offline);

        let (class, text) = match status {
            Status::History => ("history", "HISTORY"),
            Status::Offline if disabled => ("offline", "DISABLED"),
            Status::Offline => ("offline", "OFFLINE"),
            _ => ("live", "LIVE"),
        };
        for c in ["live", "history", "offline"] {
            set_class(&self.status_pill, c, c == class);
        }
        self.status_label.set_text(text);

        let playing = matches!(status, Status::Live | Status::History);
        self.toolbar.set_visible(self.hovered.get() && playing);
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(id) = self.retry.borrow_mut().take() {
            id.remove();
        }
    }
}

// --- audio: one unmuted slot at a time ----------------------------------------

thread_local! {
    /// Every live `Slot` (weak), so unmuting one can mute the others. Slots
    /// of a discarded layout drop out as their `Rc`s die.
    static SLOTS: RefCell<Vec<std::rc::Weak<Slot>>> = const { RefCell::new(Vec::new()) };
}

fn register(slot: &Rc<Slot>) {
    SLOTS.with(|s| {
        let mut v = s.borrow_mut();
        v.retain(|w| w.strong_count() > 0);
        v.push(Rc::downgrade(slot));
    });
}

fn live_slots() -> Vec<Rc<Slot>> {
    SLOTS.with(|s| s.borrow().iter().filter_map(|w| w.upgrade()).collect())
}

/// The profile whose camera tables apply: the connected one, else the default.
fn active_profile_name(store: &Store) -> String {
    store
        .get()
        .connection
        .as_ref()
        .map(|c| c.profile.clone())
        .unwrap_or_else(|| store.config().general.default_profile.clone())
}

// --- pure helpers ---------------------------------------------------------------

/// `HH:MM:SS` for the timecode pill.
pub fn format_timecode(t: &chrono::NaiveDateTime) -> String {
    t.format("%H:%M:%S").to_string()
}

/// `HH:MM:SS` (local time) for a unix-millisecond position.
pub fn format_timecode_ms(ms: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_millis_opt(ms).single() {
        Some(t) => format_timecode(&t.naive_local()),
        None => "--:--:--".into(),
    }
}

/// `<camera>-<YYYYMMDD-HHMMSS>.jpg`, with path separators and whitespace in
/// the camera name replaced so the result is a single file name.
pub fn snapshot_file_name(camera_name: &str, at: &chrono::NaiveDateTime) -> String {
    let safe: String = camera_name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_whitespace() => '_',
            c => c,
        })
        .collect();
    let safe = safe.trim_matches('_');
    let safe = if safe.is_empty() { "camera" } else { safe };
    format!("{safe}-{}.jpg", at.format("%Y%m%d-%H%M%S"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at() -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 11)
            .unwrap()
            .and_hms_opt(15, 3, 9)
            .unwrap()
    }

    #[test]
    fn timecode_is_hms() {
        assert_eq!(format_timecode(&at()), "15:03:09");
    }

    #[test]
    fn snapshot_names_are_safe_single_files() {
        assert_eq!(
            snapshot_file_name("CAM 59 - RLC823A", &at()),
            "CAM_59_-_RLC823A-20261011-150309.jpg"
        );
        assert_eq!(
            snapshot_file_name("front/door: east", &at()),
            "front_door__east-20261011-150309.jpg"
        );
        assert_eq!(
            snapshot_file_name("   ", &at()),
            "camera-20261011-150309.jpg"
        );
    }
}
