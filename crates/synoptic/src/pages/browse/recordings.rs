//! Recordings page: `Recording.List` rows (`148px 1fr auto`) with a
//! `Recording.GetThumbnail` thumbnail, detection chips, mono range, meta and
//! Play / Download. Page size 8.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use ss_api::{ApiRequest, Client, Recording, RecordingFilter};

use super::{
    Browse, Content, DownloadKind, clear_box, format_duration, format_range, format_size, hint,
    media_filename, parent_window, recording_mode, start_download,
};
use crate::icons;
use crate::widgets::controls::label;

/// `Recording.GetThumbnail` v5 for one recording (or any camera/time), as
/// the base64 JPEG the NAS sends. `None` when it has none or failed.
pub async fn fetch_thumbnail_b64(
    client: Client,
    camera_id: i64,
    arch_id: i64,
    mount_id: i64,
    target_time: i64,
) -> Option<String> {
    let event_info = format!(
        "[{{\"cameraId\":{camera_id},\"archId\":{arch_id},\"mountId\":{mount_id},\
         \"rec_group\":0,\"targetTime\":{target_time}}}]"
    );
    let req = ApiRequest::new("SYNO.SurveillanceStation.Recording", "GetThumbnail", 5)
        .param("dsId", "0")
        .param("blFallbackByLoadEvt", "true")
        .param("eventInfo", event_info);
    let data = client.send(&req).await.ok()?;
    let items = match data.as_array() {
        Some(a) => a.to_vec(),
        None => vec![data.clone()],
    };
    items.iter().find_map(|t| {
        t.get("thumbnail")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

/// Decode a base64 JPEG into a texture (main thread).
pub fn texture_from_b64(b64: &str) -> Option<gdk::Texture> {
    let bytes = glib::base64_decode(b64);
    if bytes.is_empty() {
        return None;
    }
    gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok()
}

/// 148×84 thumbnail slot with a bottom-right badge; returns the overlay and
/// the picture to fill in later.
pub fn thumb_slot(badge: &str) -> (gtk::Overlay, gtk::Picture) {
    let overlay = gtk::Overlay::new();
    overlay.add_css_class("thumb-wrap");
    overlay.set_valign(gtk::Align::Start);
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("thumb");
    frame.add_css_class("thumb-placeholder");
    frame.set_size_request(148, 84);
    frame.set_overflow(gtk::Overflow::Hidden);
    let placeholder = icons::image("play_circle", 24);
    placeholder.set_valign(gtk::Align::Center);
    placeholder.set_halign(gtk::Align::Center);
    placeholder.set_vexpand(true);
    frame.append(&placeholder);
    let picture = gtk::Picture::new();
    picture.set_size_request(148, 84);
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.set_visible(false);
    frame.append(&picture);
    overlay.set_child(Some(&frame));
    if !badge.is_empty() {
        let b = label(badge, &["duration-badge"]);
        b.set_halign(gtk::Align::End);
        b.set_valign(gtk::Align::End);
        overlay.add_overlay(&b);
    }
    (overlay, picture)
}

/// Show `texture` in a [`thumb_slot`] picture, hiding the placeholder icon.
pub fn show_thumb(picture: &gtk::Picture, texture: gdk::Texture) {
    picture.set_paintable(Some(&texture));
    picture.set_visible(true);
    if let Some(parent) = picture.parent() {
        parent.remove_css_class("thumb-placeholder");
        if let Some(first) = parent.first_child()
            && first != *picture.upcast_ref::<gtk::Widget>()
        {
            first.set_visible(false);
        }
    }
}

/// Smart-detection chip (`Person`, `Vehicle`, `Animal`, …).
pub fn detect_chip(name: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    b.add_css_class("detect-chip");
    b.set_valign(gtk::Align::Center);
    let icon = match name {
        "Person" => Some("person"),
        "Vehicle" => Some("directions_car"),
        "Animal" => Some("pets"),
        "Tampering" => Some("report"),
        _ => None,
    };
    if let Some(i) = icon {
        b.append(&icons::image(i, 12));
    }
    b.append(&gtk::Label::new(Some(name)));
    b
}

/// Video codec word for a camera (meta line), `-` when unknown.
fn codec_word(ctx: &Browse, camera_id: i64) -> String {
    ctx.store
        .get()
        .camera(camera_id)
        .map(|c| c.video_codec.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn play(ctx: &Rc<Browse>, anchor: &gtk::Widget, rec: &Recording) {
    let Some(window) = parent_window(anchor) else {
        return;
    };
    crate::dialogs::player::open(&window, &ctx.store, &ctx.services, rec.clone(), None);
}

fn row(ctx: &Rc<Browse>, rec: &Recording) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    row.add_css_class("list-row");
    row.add_css_class("clickable");

    let (thumb, picture) = thumb_slot(&format_duration(rec.duration_secs()));
    row.append(&thumb);

    let mid = gtk::Box::new(gtk::Orientation::Vertical, 4);
    mid.set_hexpand(true);
    mid.set_valign(gtk::Align::Center);
    let title = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let name = if rec.camera_name.is_empty() {
        ctx.camera_name(rec.camera_id)
            .unwrap_or_else(|| format!("Camera {}", rec.camera_id))
    } else {
        rec.camera_name.clone()
    };
    let name_label = label(&name, &["row-title"]);
    name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.append(&name_label);
    for tag in rec.detection_labels() {
        title.append(&detect_chip(tag));
    }
    mid.append(&title);
    mid.append(&label(
        &format_range(rec.start_time, rec.stop_time, " – "),
        &["row-range"],
    ));
    mid.append(&label(
        &format!(
            "{} · {} · {}",
            format_size(rec.file_size),
            codec_word(ctx, rec.camera_id),
            recording_mode(rec.event_type)
        ),
        &["row-meta"],
    ));
    row.append(&mid);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.add_css_class("row-actions");
    actions.set_valign(gtk::Align::Center);
    let play_btn = icons::bordered_button("play_arrow_fill", "icon-34", "Play");
    {
        let ctx = ctx.clone();
        let rec = rec.clone();
        play_btn.connect_clicked(move |b| play(&ctx, b.upcast_ref(), &rec));
    }
    let dl_btn = icons::bordered_button("download", "icon-34", "Download");
    {
        let ctx = ctx.clone();
        let rec = rec.clone();
        let name = name.clone();
        dl_btn.connect_clicked(move |_| {
            start_download(
                &ctx.store,
                &ctx.services,
                DownloadKind::Recording(rec.id),
                media_filename(&name, rec.start_time, "mp4"),
            );
        });
    }
    actions.append(&play_btn);
    actions.append(&dl_btn);
    row.append(&actions);

    // click anywhere on the row = play (buttons claim their own clicks)
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_PRIMARY);
    {
        let ctx = ctx.clone();
        let rec = rec.clone();
        let row_w = row.clone();
        click.connect_released(move |g, _, x, y| {
            if row_w.contains(x, y) {
                g.set_state(gtk::EventSequenceState::Claimed);
                play(&ctx, row_w.upcast_ref(), &rec);
            }
        });
    }
    row.add_controller(click);

    // thumbnail
    if let Some(client) = ctx.client() {
        let fut = ctx.services.run(fetch_thumbnail_b64(
            client,
            rec.camera_id,
            rec.arch_id,
            rec.mount_id,
            rec.start_time,
        ));
        let picture = picture.clone();
        glib::spawn_future_local(async move {
            if let Some(b64) = fut.await
                && let Some(tex) = texture_from_b64(&b64)
            {
                show_thumb(&picture, tex);
            }
        });
    }

    row.upcast()
}

pub fn build(ctx: &Rc<Browse>) -> Content {
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_vexpand(true);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.add_css_class("browse-list");
    scroller.set_child(Some(&list));

    let load: Rc<dyn Fn()> = {
        let ctx = ctx.clone();
        let list = list.clone();
        Rc::new(move || {
            let generation = ctx.generation();
            ctx.take_force();
            clear_box(&list);
            let Some(client) = ctx.client() else {
                list.append(&hint("Not connected. Recordings appear after login."));
                ctx.loaded(0);
                return;
            };
            list.append(&hint("Loading recordings…"));
            let mut filter = RecordingFilter {
                camera_ids: ctx.filter.borrow().camera.into_iter().collect(),
                ..RecordingFilter::default()
            }
            .page(ctx.offset(), ctx.page_size as u32);
            if let Some((from, to)) = ctx.time_range_unix() {
                filter = filter.between(from, to);
            }
            let fut = ctx
                .services
                .run(async move { client.list_recordings(&filter).await });
            let ctx = ctx.clone();
            let list = list.clone();
            glib::spawn_future_local(async move {
                let result = fut.await;
                if !ctx.is_current(generation) {
                    return;
                }
                clear_box(&list);
                match result {
                    Ok(page) => {
                        if page.items.is_empty() {
                            list.append(&hint("No recordings match the current filter."));
                        }
                        for rec in &page.items {
                            list.append(&row(&ctx, rec));
                        }
                        ctx.loaded(page.total);
                    }
                    Err(e) => {
                        list.append(&hint(&format!("Could not load recordings: {e}")));
                        ctx.loaded(0);
                    }
                }
            });
        })
    };

    Content {
        widget: scroller.upcast(),
        load,
    }
}
