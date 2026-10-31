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
    empty_box: gtk::Box,
    empty_hint: gtk::Label,
    offline_label: gtk::Label,

    camera: RefCell<Option<Camera>>,
    status: Cell<Status>,
    selected: Cell<bool>,
    hovered: Cell<bool>,
    muted: Cell<bool>,
    stream: RefCell<Option<VideoStream>>,
    /// History: last wall-clock position reported (unix ms).
    position_ms: Cell<Option<i64>>,
    /// Bumped whenever the stream is (re)started so stale async results are ignored.
    generation: Cell<u64>,
    retry: RefCell<Option<glib::SourceId>>,
}

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
        let mute_button = gtk::Button::new();
        let mute_icon = icons::image("volume_up", 18);
        mute_button.set_child(Some(&mute_icon));
        mute_button.set_tooltip_text(Some("Mute"));
        let snapshot_button = icons::flat_button("photo_camera", "icon-30", "Snapshot");
        let ptz_group = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        ptz_group.append(&divider_v());
        let ptz_button = icons::flat_button("control_camera", "icon-30", "PTZ pad");
        let zoom_button = icons::flat_button("zoom_in", "icon-30", "Zoom");
        let talk_button = icons::flat_button("mic", "icon-30", "Push-to-talk");
        ptz_group.append(&ptz_button);
        ptz_group.append(&zoom_button);
        ptz_group.append(&talk_button);
        let single_button = icons::flat_button("open_in_full", "icon-30", "Open in 1×1");
        toolbar.append(&mute_button);
        toolbar.append(&snapshot_button);
        toolbar.append(&ptz_group);
        toolbar.append(&divider_v());
        toolbar.append(&single_button);
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
            empty_box,
            empty_hint,
            offline_label,
            camera: RefCell::new(None),
            status: Cell::new(Status::Empty),
            selected: Cell::new(false),
            hovered: Cell::new(false),
            muted: Cell::new(false),
            stream: RefCell::new(None),
            position_ms: Cell::new(None),
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

        // click selects (click again deselects)
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        {
            let w = Rc::downgrade(&slot);
            click.connect_released(move |gesture, _, _, _| {
                let Some(s) = w.upgrade() else { return };
                gesture.set_state(gtk::EventSequenceState::Claimed);
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
        for (button, what) in [
            (&ptz_button, "PTZ control"),
            (&zoom_button, "Zoom"),
            (&talk_button, "Push-to-talk"),
        ] {
            let store = store.clone();
            button.connect_clicked(move |_| store.toast(format!("{what} not implemented")));
        }
        {
            let w = Rc::downgrade(&slot);
            single_button.connect_clicked(move |_| {
                if let Some(s) = w.upgrade()
                    && let Some(id) = s.camera_id()
                {
                    s.store.emit(&Change::RequestOpenSingle(id));
                }
            });
        }

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
            if self.camera_id().is_some() {
                self.start_live();
            }
        }
    }

    pub fn set_paused(&self, paused: bool) {
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
        let current = self.stream.borrow().as_ref().and_then(|s| s.recording());
        if let Some(rec) = current
            && rec.contains(target_secs)
        {
            if let Some(s) = &*self.stream.borrow() {
                s.seek_ms(rec.offset_ms(target_secs));
                s.set_speed(speed);
                if paused {
                    s.pause();
                }
            }
            self.position_ms.set(Some(target_secs * 1000));
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

    /// Called ~4×/s by the grid: live timecode is the local wall clock.
    pub fn tick(&self, now: chrono::DateTime<chrono::Local>) {
        match self.status.get() {
            Status::Live | Status::Connecting => {
                self.timecode.set_text(&format_timecode(&now.naive_local()));
            }
            Status::History => {
                if let Some(ms) = self.position_ms.get() {
                    self.timecode.set_text(&format_timecode_ms(ms));
                }
            }
            _ => {}
        }
    }

    // --- streams -----------------------------------------------------------

    fn auth(&self) -> Option<StreamAuth> {
        let (base_url, sid) = self.services.stream_auth()?;
        let profile = self
            .store
            .get()
            .connection
            .as_ref()
            .map(|c| c.profile.clone());
        let insecure = profile
            .and_then(|name| {
                self.store
                    .config()
                    .profiles
                    .get(&name)
                    .map(|p| !p.verify_ssl)
            })
            .unwrap_or(true);
        Some(StreamAuth {
            base_url,
            sid,
            insecure,
        })
    }

    fn stop_stream(&self) {
        self.generation.set(self.generation.get() + 1);
        if let Some(id) = self.retry.borrow_mut().take() {
            id.remove();
        }
        self.stream.borrow_mut().take();
        self.position_ms.set(None);
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
                    slot.set_status(Status::Offline);
                    slot.schedule_retry();
                }
                StreamEvent::Position(ms) => {
                    slot.position_ms.set(Some(ms));
                    if slot.status.get() == Status::Connecting {
                        slot.set_status(Status::History);
                    }
                }
            }
        });
        match opened {
            Ok(stream) => *self.stream.borrow_mut() = Some(stream),
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

    fn toggle_mute(&self) {
        let muted = !self.muted.get();
        self.muted.set(muted);
        self.mute_icon.set_icon_name(Some(&icons::name(if muted {
            "volume_off"
        } else {
            "volume_up"
        })));
        if let Some(s) = &*self.stream.borrow() {
            s.set_mute(muted);
        }
        self.store.toast(if muted {
            "Muted (audio playback not implemented)"
        } else {
            "Unmuted (audio playback not implemented)"
        });
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
        self.offline_label.set_visible(status == Status::Offline);
        self.picture.set_visible(status != Status::Offline);

        let (class, text) = match status {
            Status::History => ("history", "HISTORY"),
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
