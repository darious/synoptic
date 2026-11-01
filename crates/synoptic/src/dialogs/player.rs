//! Player dialog (`min(1100px, 90%)`): header title + mono subtitle,
//! Download, ✕; 16:9 video; transport row with play/pause, mono position,
//! seek bar, duration, Volume, Snapshot.
//!
//! Recordings and events play over the MixStream WebSocket (`stmSrc=2`,
//! then `action=play … stmSrc=1`), the same path the Live View uses for
//! history and the one the Synology client takes: the HTTP `EventStream`
//! URL is not what the official client plays and some DSM builds answer
//! it with an empty body. Time-lapse files still go through `playbin3` on
//! `Client::recording_stream_url`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gstreamer as gst;
use gstreamer::prelude::*;
use gtk::prelude::*;
use gtk::{gdk, glib};
use ss_api::Recording;
use ss_stream::Speed;

use crate::dialogs::shell;
use crate::icons;
use crate::pages::browse::{
    DownloadKind, format_duration, format_local, format_range, media_filename, snapshot_dir,
    start_download,
};
use crate::services::Services;
use crate::state::Store;
use crate::video::bridge::{HistoryRecording, StreamAuth, StreamEvent, StreamSource, VideoStream};
use crate::video::pipeline::Codec;
use crate::widgets::controls::label;

/// Dialog width for a parent window: `min(1100, 90 %)`.
pub fn dialog_width(parent_width: i32) -> i32 {
    if parent_width <= 0 {
        return 1100;
    }
    ((parent_width as f64) * 0.9).round().min(1100.0) as i32
}

/// `Recording.event_type` of a time-lapse file (`to_recording`).
const TIME_LAPSE: i64 = 3;

enum Backend {
    /// Recordings and events: MixStream history session.
    Mix(RefCell<Option<VideoStream>>),
    /// Time-lapse: `playbin3` on the HTTP stream URL.
    Playbin(gst::Element),
    /// Could not be built; the reason is on screen.
    None,
}

struct Player {
    store: Rc<Store>,
    rec: Recording,
    backend: Backend,
    paintable: RefCell<Option<gdk::Paintable>>,
    picture: gtk::Picture,
    play_btn: gtk::Button,
    scale: gtk::Scale,
    position: gtk::Label,
    duration: gtk::Label,
    timecode: gtk::Label,
    notice: gtk::Label,
    playing: Cell<bool>,
    duration_secs: Cell<f64>,
    position_secs: Cell<f64>,
    volume: Cell<f64>,
    /// Playbin: seek to apply once prerolled.
    pending_seek: Cell<Option<f64>>,
    bus_guard: RefCell<Option<gst::bus::BusWatchGuard>>,
    timer: RefCell<Option<glib::SourceId>>,
}

impl Player {
    fn set_playing(&self, on: bool) {
        self.playing.set(on);
        let icon = if on { "pause-fill" } else { "play_arrow-fill" };
        self.play_btn.set_child(Some(&icons::image(icon, 16)));
        self.play_btn
            .set_tooltip_text(Some(if on { "Pause" } else { "Play" }));
        match &self.backend {
            Backend::Mix(stream) => {
                if let Some(s) = &*stream.borrow() {
                    if on {
                        s.resume();
                    } else {
                        s.pause();
                    }
                }
            }
            Backend::Playbin(p) => {
                let state = if on {
                    gst::State::Playing
                } else {
                    gst::State::Paused
                };
                if let Err(e) = p.set_state(state) {
                    tracing::warn!("player state change: {e}");
                }
            }
            Backend::None => {}
        }
    }

    fn toggle(&self) {
        if matches!(self.backend, Backend::None) {
            return;
        }
        let at_end = self.position_secs.get() >= self.duration_secs.get() - 0.5;
        if !self.playing.get() && at_end {
            self.seek(0.0);
        }
        self.set_playing(!self.playing.get());
    }

    fn seek(&self, secs: f64) {
        let secs = secs.clamp(0.0, self.duration_secs.get().max(0.0));
        match &self.backend {
            Backend::Mix(stream) => {
                if let Some(s) = &*stream.borrow() {
                    s.seek_ms((secs * 1000.0) as u64);
                }
            }
            Backend::Playbin(p) => {
                if p.seek_simple(
                    gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                    gst::ClockTime::from_seconds_f64(secs),
                )
                .is_err()
                {
                    // Not seekable yet (still prerolling): retry when ready.
                    self.pending_seek.set(Some(secs));
                }
            }
            Backend::None => return,
        }
        self.show_position(secs);
    }

    fn show_position(&self, secs: f64) {
        self.position_secs.set(secs);
        self.position.set_text(&format_duration(secs as i64));
        self.timecode
            .set_text(&format_local(self.rec.start_time + secs as i64));
        if !self.scale.has_focus() {
            self.scale.set_value(secs);
        }
    }

    fn set_duration(&self, secs: f64) {
        if secs <= 0.0 || (secs - self.duration_secs.get()).abs() < 0.5 {
            return;
        }
        self.duration_secs.set(secs);
        self.scale.set_range(0.0, secs);
        self.duration.set_text(&format_duration(secs as i64));
    }

    /// Timer tick: pull position / duration from a playbin pipeline.
    fn tick(&self) {
        let Backend::Playbin(p) = &self.backend else {
            return;
        };
        if let Some(d) = p.query_duration::<gst::ClockTime>() {
            self.set_duration(d.seconds_f64());
        }
        if self.playing.get()
            && let Some(pos) = p.query_position::<gst::ClockTime>()
        {
            self.show_position(pos.seconds_f64());
        }
    }

    /// A line over the video ("Connecting…"); `None` hides it.
    fn set_notice(&self, text: Option<&str>) {
        match text {
            Some(t) => {
                self.notice.set_text(t);
                self.notice.set_visible(true);
            }
            None => self.notice.set_visible(false),
        }
    }

    fn show_error(&self, text: &str) {
        self.set_notice(Some(text));
        self.set_playing(false);
    }

    fn set_volume(&self, volume: f64) {
        self.volume.set(volume);
        match &self.backend {
            Backend::Mix(stream) => {
                if let Some(s) = &*stream.borrow() {
                    s.set_volume(volume);
                    s.set_mute(volume <= 0.0);
                }
            }
            Backend::Playbin(p) => p.set_property("volume", volume),
            Backend::None => {}
        }
    }

    /// Open the MixStream history session for the recording, starting
    /// `offset` seconds in.
    fn start_mix(self: &Rc<Self>, services: &Services, offset: f64) {
        let Backend::Mix(slot) = &self.backend else {
            return;
        };
        let Some(auth) = StreamAuth::from_session(&self.store, services) else {
            self.show_error("Not connected");
            return;
        };
        let recording = HistoryRecording::from_api(&self.rec);
        let codec = self
            .store
            .get()
            .camera(self.rec.camera_id)
            .map(|c| Codec::from_camera(c.video_codec))
            .unwrap_or(Codec::H264);
        let target = self.rec.start_time + offset.max(0.0) as i64;
        let params = recording.play_params(target, Speed::default(), false);
        let w = Rc::downgrade(self);
        let opened = VideoStream::open(
            services.runtime(),
            &auth,
            StreamSource::History { recording, params },
            codec,
            move |ev| {
                if let Some(p) = w.upgrade() {
                    p.on_stream_event(ev);
                }
            },
        );
        match opened {
            Ok(stream) => {
                stream.set_mute(self.volume.get() <= 0.0);
                stream.set_volume(self.volume.get());
                *slot.borrow_mut() = Some(stream);
                self.show_position(offset.max(0.0));
                self.set_notice(Some("Connecting…"));
                self.set_playing(true);
            }
            Err(e) => {
                tracing::warn!("player stream URL: {e}");
                self.show_error(&format!("Playback failed: {e}"));
            }
        }
    }

    fn on_stream_event(&self, ev: StreamEvent) {
        match ev {
            StreamEvent::Paintable(p) => {
                self.picture.set_paintable(Some(&p));
                *self.paintable.borrow_mut() = Some(p);
            }
            StreamEvent::Playing => self.set_notice(None),
            StreamEvent::Reconnecting(why) => {
                tracing::info!("player: {why}");
                self.set_notice(Some("Reconnecting…"));
            }
            StreamEvent::Offline(why) => {
                tracing::warn!("player: offline: {why}");
                if self.position_secs.get() >= self.duration_secs.get() - 3.0 {
                    self.ended();
                } else {
                    self.show_error(&format!("Playback failed: {why}"));
                }
            }
            StreamEvent::Position(ms) => {
                let secs = (ms as f64 / 1000.0 - self.rec.start_time as f64).max(0.0);
                self.set_notice(None);
                if self.playing.get() {
                    self.show_position(secs);
                }
            }
            StreamEvent::Ended => self.ended(),
        }
    }

    fn ended(&self) {
        self.set_notice(None);
        self.set_playing(false);
        self.show_position(self.duration_secs.get());
    }

    /// Save the current frame as PNG under the configured snapshot dir.
    fn snapshot(&self) {
        let Some(paintable) = self.paintable.borrow().clone() else {
            self.store.toast("No video frame yet");
            return;
        };
        let image = paintable.current_image();
        let (w, h) = (image.intrinsic_width(), image.intrinsic_height());
        if w <= 0 || h <= 0 {
            self.store.toast("No video frame yet");
            return;
        }
        let texture = match image.clone().downcast::<gdk::Texture>() {
            Ok(t) => Some(t),
            Err(_) => {
                let snap = gtk::Snapshot::new();
                image.snapshot(&snap, w as f64, h as f64);
                snap.to_node().and_then(|node| {
                    self.picture
                        .native()
                        .and_then(|n| n.renderer())
                        .map(|r| r.render_texture(&node, None))
                })
            }
        };
        let Some(texture) = texture else {
            self.store.toast("Could not capture the frame");
            return;
        };
        let dir = snapshot_dir(&self.store);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.store
                .toast(format!("Cannot create {}: {e}", dir.display()));
            return;
        }
        let at = self.rec.start_time + self.scale.value() as i64;
        let path = dir.join(media_filename(&self.rec.camera_name, at, "png"));
        match texture.save_to_png(&path) {
            Ok(()) => self
                .store
                .toast(format!("Snapshot saved · {}", self.rec.camera_name)),
            Err(e) => self.store.toast(format!("Snapshot failed: {e}")),
        }
    }

    fn shutdown(&self) {
        if let Some(t) = self.timer.borrow_mut().take() {
            t.remove();
        }
        self.bus_guard.borrow_mut().take();
        match &self.backend {
            Backend::Mix(stream) => {
                stream.borrow_mut().take();
            }
            Backend::Playbin(p) => {
                let _ = p.set_state(gst::State::Null);
            }
            Backend::None => {}
        }
    }
}

/// Build the time-lapse pipeline; `Err` carries a user-facing reason.
fn build_pipeline(url: &str) -> Result<(gst::Element, gdk::Paintable), String> {
    let sink = gst::ElementFactory::make("gtk4paintablesink")
        .build()
        .map_err(|_| "gtk4paintablesink is not installed (gstreamer1.0-gtk4)".to_string())?;
    let paintable = sink.property::<gdk::Paintable>("paintable");
    let playbin = gst::ElementFactory::make("playbin3")
        .property("video-sink", &sink)
        .property("uri", url)
        .build()
        .map_err(|_| "playbin3 is not available".to_string())?;
    Ok((playbin, paintable))
}

/// Open the player for `rec`, optionally seeking to `start_offset` seconds.
pub fn open(
    parent: &gtk::Window,
    store: &Rc<Store>,
    services: &Rc<Services>,
    rec: Recording,
    start_offset: Option<f64>,
) {
    let width = dialog_width(parent.width());
    let title = if rec.camera_name.is_empty() {
        store
            .get()
            .camera(rec.camera_id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| format!("Camera {}", rec.camera_id))
    } else {
        rec.camera_name.clone()
    };
    let rec = Recording {
        camera_name: title.clone(),
        ..rec
    };
    let subtitle = format!(
        "{} · ID {}",
        format_range(rec.start_time, rec.stop_time, " – "),
        rec.id
    );
    let dialog = shell::present(parent, width, "play_circle", &title, Some(&subtitle));
    dialog.body.remove_css_class("dialog-body");
    dialog.body.add_css_class("player-body");
    dialog.body.set_spacing(0);

    // ----- video area ------------------------------------------------------
    let frame = gtk::AspectFrame::new(0.5, 0.5, 16.0 / 9.0, false);
    frame.add_css_class("player-video");
    let overlay = gtk::Overlay::new();
    let picture = gtk::Picture::new();
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    overlay.set_child(Some(&picture));
    let timecode = label(&format_local(rec.start_time), &["timecode"]);
    timecode.set_halign(gtk::Align::End);
    timecode.set_valign(gtk::Align::End);
    overlay.add_overlay(&timecode);
    let notice = label("", &["player-error"]);
    notice.set_widget_name("player-notice");
    notice.set_halign(gtk::Align::Center);
    notice.set_valign(gtk::Align::Center);
    notice.set_visible(false);
    notice.set_wrap(true);
    overlay.add_overlay(&notice);
    frame.set_child(Some(&overlay));
    dialog.body.append(&frame);

    // ----- transport -------------------------------------------------------
    let transport = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    transport.add_css_class("transport");
    let play_btn = gtk::Button::new();
    play_btn.add_css_class("play");
    play_btn.add_css_class("icon-34");
    play_btn.set_widget_name("player-play");
    play_btn.set_valign(gtk::Align::Center);
    transport.append(&play_btn);
    let position = label("0:00", &["position"]);
    position.set_width_chars(6);
    transport.append(&position);
    let scale = gtk::Scale::with_range(
        gtk::Orientation::Horizontal,
        0.0,
        rec.duration_secs().max(1) as f64,
        1.0,
    );
    scale.add_css_class("seek");
    scale.set_draw_value(false);
    scale.set_hexpand(true);
    scale.set_valign(gtk::Align::Center);
    transport.append(&scale);
    let duration = label(&format_duration(rec.duration_secs()), &["duration"]);
    duration.set_width_chars(6);
    transport.append(&duration);

    let volume = gtk::MenuButton::new();
    volume.set_icon_name(&icons::name("volume_up"));
    volume.add_css_class("icon-30");
    volume.set_tooltip_text(Some("Volume"));
    volume.set_valign(gtk::Align::Center);
    let vol_pop = gtk::Popover::new();
    vol_pop.add_css_class("volume-pop");
    let vol_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.05);
    vol_scale.set_value(1.0);
    vol_scale.set_draw_value(false);
    vol_scale.set_size_request(140, -1);
    vol_pop.set_child(Some(&vol_scale));
    volume.set_popover(Some(&vol_pop));
    transport.append(&volume);

    let snap_btn = icons::bordered_button("photo_camera", "icon-30", "Snapshot");
    transport.append(&snap_btn);
    dialog.body.append(&transport);

    // ----- header button (Download) before ✕ ----------------------------
    let download = icons::bordered_button("download", "icon-32", "Download");
    download.set_widget_name("player-download");
    if let Some(header) = dialog.body.prev_sibling().and_downcast::<gtk::Box>() {
        header.add_css_class("h44");
        if let Some(spacer) = header.last_child().and_then(|c| c.prev_sibling()) {
            header.insert_child_after(&download, Some(&spacer));
        }
    }

    // ----- backend ----------------------------------------------------------
    let time_lapse = rec.event_type == TIME_LAPSE;
    let mut init_paintable = None;
    let (backend, init_error) = if !time_lapse {
        (Backend::Mix(RefCell::new(None)), None)
    } else {
        let url = services
            .client()
            .map(|c| c.recording_stream_url(&rec).to_string());
        match url {
            None => (Backend::None, Some("Not connected".to_string())),
            Some(url) => match build_pipeline(&url) {
                Ok((p, paintable)) => {
                    picture.set_paintable(Some(&paintable));
                    init_paintable = Some(paintable);
                    (Backend::Playbin(p), None)
                }
                Err(e) => (Backend::None, Some(e)),
            },
        }
    };

    let player = Rc::new(Player {
        store: store.clone(),
        rec: rec.clone(),
        backend,
        paintable: RefCell::new(init_paintable),
        picture: picture.clone(),
        play_btn: play_btn.clone(),
        scale: scale.clone(),
        position,
        duration,
        timecode,
        notice,
        playing: Cell::new(false),
        duration_secs: Cell::new(rec.duration_secs().max(1) as f64),
        position_secs: Cell::new(0.0),
        volume: Cell::new(1.0),
        pending_seek: Cell::new(start_offset.filter(|o| *o > 0.0)),
        bus_guard: RefCell::new(None),
        timer: RefCell::new(None),
    });

    if let Some(e) = init_error {
        player.show_error(&e);
    }

    // playbin bus: errors, EOS, async-done (pending seek)
    if let Backend::Playbin(p) = &player.backend
        && let Some(bus) = p.bus()
    {
        let weak = Rc::downgrade(&player);
        let guard = bus.add_watch_local(move |_, msg| {
            let Some(player) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            match msg.view() {
                gst::MessageView::Error(err) => {
                    // `err.error()` is the GError text only: never the URL
                    // (which carries the session id).
                    tracing::warn!("playback error: {}", err.error());
                    player.show_error(&format!("Playback failed: {}", err.error()));
                }
                gst::MessageView::Eos(_) => player.ended(),
                gst::MessageView::AsyncDone(_) => {
                    player.tick();
                    if let Some(secs) = player.pending_seek.take() {
                        player.seek(secs);
                    }
                }
                _ => {}
            }
            glib::ControlFlow::Continue
        });
        match guard {
            Ok(g) => *player.bus_guard.borrow_mut() = Some(g),
            Err(e) => tracing::warn!("bus watch: {e}"),
        }

        // position timer
        let weak = Rc::downgrade(&player);
        let id = glib::timeout_add_local(std::time::Duration::from_millis(250), move || match weak
            .upgrade()
        {
            Some(p) => {
                p.tick();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        *player.timer.borrow_mut() = Some(id);
    }

    // controls
    {
        let p = player.clone();
        play_btn.connect_clicked(move |_| p.toggle());
    }
    {
        let p = player.clone();
        scale.connect_change_value(move |_, _, value| {
            p.seek(value);
            glib::Propagation::Proceed
        });
    }
    {
        let p = player.clone();
        vol_scale.connect_value_changed(move |s| p.set_volume(s.value()));
    }
    {
        let p = player.clone();
        snap_btn.connect_clicked(move |_| p.snapshot());
    }
    {
        let store = store.clone();
        let services = services.clone();
        let rec = rec.clone();
        download.connect_clicked(move |_| {
            let kind = if rec.event_type == TIME_LAPSE {
                DownloadKind::TimeLapse(rec.id)
            } else {
                DownloadKind::Recording(rec.id)
            };
            start_download(
                &store,
                &services,
                kind,
                media_filename(&rec.camera_name, rec.start_time, "mp4"),
            );
        });
    }

    // Space toggles play/pause.
    {
        let p = player.clone();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::space {
                p.toggle();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.window.add_controller(keys);
    }

    // tear down with the dialog
    {
        let p = player.clone();
        dialog.window.connect_closed(move || p.shutdown());
    }

    // go
    match &player.backend {
        Backend::Mix(_) => player.start_mix(services, start_offset.unwrap_or(0.0)),
        Backend::Playbin(_) => player.set_playing(true),
        Backend::None => player.set_playing(false),
    }
}

#[cfg(test)]
mod tests {
    use super::dialog_width;

    #[test]
    fn width_rule() {
        assert_eq!(dialog_width(0), 1100);
        assert_eq!(dialog_width(1000), 900);
        assert_eq!(dialog_width(1280), 1100);
        assert_eq!(dialog_width(1200), 1080);
    }
}
