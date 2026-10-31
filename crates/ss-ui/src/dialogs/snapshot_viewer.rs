//! Snapshot viewer: the Player's shell with Download, Delete, ✕ and a 16:9
//! image area with scroll-zoom and drag-pan.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use ss_api::Snapshot;

use crate::dialogs::player::dialog_width;
use crate::dialogs::shell;
use crate::icons;
use crate::pages::browse::{DownloadKind, format_local, media_filename, start_download};
use crate::services::Services;
use crate::state::Store;
use crate::widgets::controls::label;

const MIN_ZOOM: f64 = 1.0;
const MAX_ZOOM: f64 = 8.0;

/// Zoom / pan of the image inside a fixed viewport. Pure so it can be
/// tested: `pan` is the image origin in viewport pixels, `zoom` ≥ 1 keeps
/// the image at least viewport-sized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub zoom: f64,
    pub pan: (f64, f64),
}

impl Default for View {
    fn default() -> Self {
        View {
            zoom: 1.0,
            pan: (0.0, 0.0),
        }
    }
}

impl View {
    /// Keep the image covering the `(w, h)` viewport.
    pub fn clamped(self, w: f64, h: f64) -> View {
        let zoom = self.zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let min_x = w - zoom * w;
        let min_y = h - zoom * h;
        View {
            zoom,
            pan: (self.pan.0.clamp(min_x, 0.0), self.pan.1.clamp(min_y, 0.0)),
        }
    }

    /// Zoom by `factor` keeping the image point under `(px, py)` fixed.
    pub fn zoomed(self, factor: f64, px: f64, py: f64, w: f64, h: f64) -> View {
        let zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let ratio = zoom / self.zoom;
        View {
            zoom,
            pan: (
                px - (px - self.pan.0) * ratio,
                py - (py - self.pan.1) * ratio,
            ),
        }
        .clamped(w, h)
    }

    pub fn panned(self, dx: f64, dy: f64, w: f64, h: f64) -> View {
        View {
            zoom: self.zoom,
            pan: (self.pan.0 + dx, self.pan.1 + dy),
        }
        .clamped(w, h)
    }
}

fn apply(fixed: &gtk::Fixed, picture: &gtk::Picture, view: View) {
    let t = gsk::Transform::new()
        .translate(&graphene::Point::new(view.pan.0 as f32, view.pan.1 as f32))
        .scale(view.zoom as f32, view.zoom as f32);
    fixed.set_child_transform(picture, Some(&t));
}

/// Open the viewer for `snap`. `thumb` is shown until the full image is
/// loaded; `on_deleted` runs after a successful delete (removes the card).
pub fn open(
    parent: &gtk::Window,
    store: &Rc<Store>,
    services: &Rc<Services>,
    snap: Snapshot,
    thumb: Option<gdk::Texture>,
    on_deleted: Rc<dyn Fn()>,
) {
    let width = dialog_width(parent.width());
    let subtitle = format!("{} · ID {}", format_local(snap.create_time), snap.id);
    let title = if snap.camera_name.is_empty() {
        "Snapshot".to_string()
    } else {
        snap.camera_name.clone()
    };
    let dialog = shell::present(parent, width, "photo_library", &title, Some(&subtitle));
    dialog.body.remove_css_class("dialog-body");
    dialog.body.add_css_class("player-body");
    dialog.body.set_spacing(0);

    // ----- image area (16:9 viewport, overflow hidden) --------------------
    let vw = (width - 2).max(320) as f64;
    let vh = (vw * 9.0 / 16.0).round();
    let fixed = gtk::Fixed::new();
    fixed.add_css_class("viewer-area");
    fixed.set_size_request(vw as i32, vh as i32);
    fixed.set_overflow(gtk::Overflow::Hidden);
    fixed.set_cursor_from_name(Some("grab"));
    let picture = gtk::Picture::new();
    picture.set_size_request(vw as i32, vh as i32);
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    if let Some(t) = &thumb {
        picture.set_paintable(Some(t));
    }
    fixed.put(&picture, 0.0, 0.0);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&fixed));
    let zoom_label = label("100%", &["timecode", "viewer-zoom"]);
    zoom_label.set_halign(gtk::Align::Start);
    zoom_label.set_valign(gtk::Align::End);
    overlay.add_overlay(&zoom_label);
    dialog.body.append(&overlay);

    let view = Rc::new(Cell::new(View::default()));
    let pointer = Rc::new(Cell::new((vw / 2.0, vh / 2.0)));

    // track the pointer for zoom-around-cursor
    {
        let motion = gtk::EventControllerMotion::new();
        let pointer = pointer.clone();
        motion.connect_motion(move |_, x, y| pointer.set((x, y)));
        fixed.add_controller(motion);
    }
    // scroll = zoom
    {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let (view, pointer) = (view.clone(), pointer.clone());
        let (fixed_w, picture_w, zoom_label) = (fixed.clone(), picture.clone(), zoom_label.clone());
        scroll.connect_scroll(move |_, _, dy| {
            let factor = if dy < 0.0 { 1.15 } else { 1.0 / 1.15 };
            let (px, py) = pointer.get();
            let v = view.get().zoomed(factor, px, py, vw, vh);
            view.set(v);
            apply(&fixed_w, &picture_w, v);
            zoom_label.set_text(&format!("{}%", (v.zoom * 100.0).round() as i64));
            glib::Propagation::Stop
        });
        fixed.add_controller(scroll);
    }
    // drag = pan
    {
        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        let start = Rc::new(Cell::new(View::default()));
        {
            let (view, start) = (view.clone(), start.clone());
            let fixed_w = fixed.clone();
            drag.connect_drag_begin(move |_, _, _| {
                start.set(view.get());
                fixed_w.set_cursor_from_name(Some("grabbing"));
            });
        }
        {
            let (view, start) = (view.clone(), start.clone());
            let (fixed_w, picture_w) = (fixed.clone(), picture.clone());
            drag.connect_drag_update(move |_, dx, dy| {
                let v = start.get().panned(dx, dy, vw, vh);
                view.set(v);
                apply(&fixed_w, &picture_w, v);
            });
        }
        {
            let fixed_w = fixed.clone();
            drag.connect_drag_end(move |_, _, _| fixed_w.set_cursor_from_name(Some("grab")));
        }
        fixed.add_controller(drag);
    }
    // double click = reset
    {
        let click = gtk::GestureClick::new();
        let (view, fixed_w, picture_w, zoom_label) = (
            view.clone(),
            fixed.clone(),
            picture.clone(),
            zoom_label.clone(),
        );
        click.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                let v = View::default();
                view.set(v);
                apply(&fixed_w, &picture_w, v);
                zoom_label.set_text("100%");
            }
        });
        fixed.add_controller(click);
    }

    // ----- header buttons: Download, Delete before ✕ --------------------
    let download = icons::bordered_button("download", "icon-32", "Download");
    let delete = icons::bordered_button("delete", "icon-32", "Delete");
    delete.add_css_class("danger");
    if let Some(header) = dialog.body.prev_sibling().and_downcast::<gtk::Box>() {
        header.add_css_class("h44");
        if let Some(spacer) = header.last_child().and_then(|c| c.prev_sibling()) {
            header.insert_child_after(&delete, Some(&spacer));
            header.insert_child_after(&download, Some(&spacer));
        }
    }
    {
        let store = store.clone();
        let services = services.clone();
        let snap = snap.clone();
        download.connect_clicked(move |_| {
            start_download(
                &store,
                &services,
                DownloadKind::Snapshot(snap.id),
                media_filename(&snap.camera_name, snap.create_time, "jpg"),
            );
        });
    }
    {
        let store = store.clone();
        let services = services.clone();
        let window = dialog.window.clone();
        let id = snap.id;
        delete.connect_clicked(move |b| {
            let Some(client) = services.client() else {
                store.toast("Not connected");
                return;
            };
            b.set_sensitive(false);
            let fut = services.run(async move { client.delete_snapshot(id, 0).await });
            let (store, window, on_deleted, b) =
                (store.clone(), window.clone(), on_deleted.clone(), b.clone());
            glib::spawn_future_local(async move {
                match fut.await {
                    Ok(()) => {
                        on_deleted();
                        store.toast("Snapshot deleted");
                        window.close();
                    }
                    Err(e) => {
                        b.set_sensitive(true);
                        store.toast(format!("Delete failed: {e}"));
                    }
                }
            });
        });
    }

    // ----- full-size image -----------------------------------------------
    if let Some(client) = services.client() {
        let id = snap.id;
        let fut = services.run(async move { client.snapshot_image(id).await });
        let store = store.clone();
        glib::spawn_future_local(async move {
            match fut.await {
                Ok(bytes) => {
                    match gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes.to_vec())) {
                        Ok(tex) => picture.set_paintable(Some(&tex)),
                        Err(e) => store.toast(format!("Cannot decode snapshot: {e}")),
                    }
                }
                Err(e) => store.toast(format!("Cannot load snapshot: {e}")),
            }
        });
    } else if thumb.is_none() {
        store.toast("Not connected");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_and_pan_stay_inside_viewport() {
        let (w, h) = (1600.0, 900.0);
        let v = View::default();
        // Zooming out below 1 is a no-op.
        assert_eq!(v.zoomed(0.5, 800.0, 450.0, w, h), View::default());
        // Zoom ×2 around the centre keeps the centre fixed.
        let z = v.zoomed(2.0, 800.0, 450.0, w, h);
        assert_eq!(z.zoom, 2.0);
        assert_eq!(z.pan, (-800.0, -450.0));
        // Panning cannot expose the background.
        let p = z.panned(5000.0, 5000.0, w, h);
        assert_eq!(p.pan, (0.0, 0.0));
        let p = z.panned(-5000.0, -5000.0, w, h);
        assert_eq!(p.pan, (-1600.0, -900.0));
        // Zoom around the top-left corner keeps the corner.
        let c = v.zoomed(3.0, 0.0, 0.0, w, h);
        assert_eq!(c.pan, (0.0, 0.0));
        // Max zoom is capped.
        let m = v.zoomed(100.0, 0.0, 0.0, w, h);
        assert_eq!(m.zoom, MAX_ZOOM);
    }
}
