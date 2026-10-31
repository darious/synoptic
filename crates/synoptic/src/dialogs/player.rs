//! Player dialog (`min(1100px, 90%)`): header title + mono subtitle,
//! Download, ✕; 16:9 video via GStreamer `playbin3` →
//! `gtk4paintablesink` playing `Client::recording_stream_url`; transport
//! row with play/pause, mono position, seek bar, duration, Volume, Snapshot.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gstreamer as gst;
use gstreamer::prelude::*;
use gtk::prelude::*;
use gtk::{gdk, glib};
use ss_api::Recording;

use crate::dialogs::shell;
use crate::icons;
use crate::pages::browse::{
    DownloadKind, format_duration, format_local, format_range, media_filename, snapshot_dir,
    start_download,
};
use crate::services::Services;
use crate::state::Store;
use crate::widgets::controls::label;

/// Dialog width for a parent window: `min(1100, 90 %)`.
pub fn dialog_width(parent_width: i32) -> i32 {
    if parent_width <= 0 {
        return 1100;
    }
    ((parent_width as f64) * 0.9).round().min(1100.0) as i32
}

struct Player {
    store: Rc<Store>,
    rec: Recording,
    playbin: Option<gst::Element>,
    paintable: Option<gdk::Paintable>,
    picture: gtk::Picture,
    play_btn: gtk::Button,
    scale: gtk::Scale,
    position: gtk::Label,
    duration: gtk::Label,
    timecode: gtk::Label,
    error: gtk::Label,
    playing: Cell<bool>,
    duration_secs: Cell<f64>,
    pending_seek: Cell<Option<f64>>,
    bus_guard: RefCell<Option<gst::bus::BusWatchGuard>>,
    timer: RefCell<Option<glib::SourceId>>,
}

impl Player {
    fn set_playing(&self, on: bool) {
        self.playing.set(on);
        let icon = if on { "pause_fill" } else { "play_arrow_fill" };
        self.play_btn.set_child(Some(&icons::image(icon, 16)));
        self.play_btn
            .set_tooltip_text(Some(if on { "Pause" } else { "Play" }));
        if let Some(p) = &self.playbin {
            let state = if on {
                gst::State::Playing
            } else {
                gst::State::Paused
            };
            if let Err(e) = p.set_state(state) {
                tracing::warn!("player state change: {e}");
            }
        }
    }

    fn toggle(&self) {
        self.set_playing(!self.playing.get());
    }

    fn seek(&self, secs: f64) {
        let Some(p) = &self.playbin else { return };
        let secs = secs.clamp(0.0, self.duration_secs.get().max(0.0));
        if p.seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_seconds_f64(secs),
        )
        .is_err()
        {
            // Not seekable yet (still prerolling): retry when ready.
            self.pending_seek.set(Some(secs));
        }
        self.show_position(secs);
    }

    fn show_position(&self, secs: f64) {
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

    /// Timer tick: pull position / duration from the pipeline.
    fn tick(&self) {
        let Some(p) = &self.playbin else { return };
        if let Some(d) = p.query_duration::<gst::ClockTime>() {
            self.set_duration(d.seconds_f64());
        }
        if self.playing.get()
            && let Some(pos) = p.query_position::<gst::ClockTime>()
        {
            self.show_position(pos.seconds_f64());
        }
    }

    fn show_error(&self, text: &str) {
        self.error.set_text(text);
        self.error.set_visible(true);
        self.set_playing(false);
    }

    /// Save the current frame as PNG under the configured snapshot dir.
    fn snapshot(&self) {
        let Some(paintable) = &self.paintable else {
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
        if let Some(p) = &self.playbin {
            let _ = p.set_state(gst::State::Null);
        }
    }
}

/// Build the pipeline; `Err` carries a user-facing reason.
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
    let error = label("", &["player-error"]);
    error.set_halign(gtk::Align::Center);
    error.set_valign(gtk::Align::Center);
    error.set_visible(false);
    error.set_wrap(true);
    overlay.add_overlay(&error);
    frame.set_child(Some(&overlay));
    dialog.body.append(&frame);

    // ----- transport -------------------------------------------------------
    let transport = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    transport.add_css_class("transport");
    let play_btn = gtk::Button::new();
    play_btn.add_css_class("play");
    play_btn.add_css_class("icon-34");
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

    // ----- pipeline ---------------------------------------------------------
    let url = services
        .client()
        .map(|c| c.recording_stream_url(&rec).to_string());
    let (playbin, paintable, init_error) = match url {
        None => (None, None, Some("Not connected".to_string())),
        Some(url) => match build_pipeline(&url) {
            Ok((p, paintable)) => (Some(p), Some(paintable), None),
            Err(e) => (None, None, Some(e)),
        },
    };
    if let Some(p) = &paintable {
        picture.set_paintable(Some(p));
    }

    let player = Rc::new(Player {
        store: store.clone(),
        rec: rec.clone(),
        playbin,
        paintable,
        picture: picture.clone(),
        play_btn: play_btn.clone(),
        scale: scale.clone(),
        position,
        duration,
        timecode,
        error,
        playing: Cell::new(false),
        duration_secs: Cell::new(rec.duration_secs().max(1) as f64),
        pending_seek: Cell::new(start_offset.filter(|o| *o > 0.0)),
        bus_guard: RefCell::new(None),
        timer: RefCell::new(None),
    });

    if let Some(e) = init_error {
        player.show_error(&e);
    }

    // bus: errors, EOS, async-done (pending seek)
    if let Some(bus) = player.playbin.as_ref().and_then(|p| p.bus()) {
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
                gst::MessageView::Eos(_) => {
                    player.set_playing(false);
                    player.show_position(player.duration_secs.get());
                }
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
    }

    // position timer
    {
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
        vol_scale.connect_value_changed(move |s| {
            if let Some(pb) = &p.playbin {
                pb.set_property("volume", s.value());
            }
        });
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
            let kind = if rec.event_type == 3 {
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

    // tear down with the window
    {
        let p = player.clone();
        dialog.window.connect_close_request(move |_| {
            p.shutdown();
            glib::Propagation::Proceed
        });
    }
    {
        let p = player.clone();
        dialog.window.connect_destroy(move |_| p.shutdown());
    }

    // go
    if player.playbin.is_some() {
        player.set_playing(true);
    } else {
        player.set_playing(false);
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
