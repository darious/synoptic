//! Snapshots page: responsive card grid (`gtk::FlowBox`, min 260 px) of
//! `SnapShot.List` entries with a 16:9 thumbnail and View / Download /
//! Delete. Page size 9.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use ss_api::{Snapshot, SnapshotFilter};

use super::recordings::texture_from_b64;
use super::{
    Browse, Content, DownloadKind, format_local, hint, media_filename, parent_window,
    start_download,
};
use crate::icons;
use crate::widgets::controls::{label, spacer};

/// A snapshot's thumbnail texture: the inline base64 from the list, else
/// fetched with `SnapShot.Download`.
fn load_texture(ctx: &Rc<Browse>, snap: &Snapshot, picture: gtk::Picture) {
    if !snap.image_data.is_empty()
        && let Some(tex) = texture_from_b64(&snap.image_data)
    {
        picture.set_paintable(Some(&tex));
        return;
    }
    let Some(client) = ctx.client() else { return };
    let id = snap.id;
    let fut = ctx
        .services
        .run(async move { client.snapshot_image(id).await.ok() });
    glib::spawn_future_local(async move {
        if let Some(bytes) = fut.await
            && let Ok(tex) = gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes.to_vec()))
        {
            picture.set_paintable(Some(&tex));
        }
    });
}

fn open_viewer(
    ctx: &Rc<Browse>,
    anchor: &gtk::Widget,
    snap: &Snapshot,
    picture: &gtk::Picture,
    on_deleted: Rc<dyn Fn()>,
) {
    let Some(window) = parent_window(anchor) else {
        return;
    };
    let thumb = picture
        .paintable()
        .and_then(|p| p.downcast::<gdk::Texture>().ok());
    crate::dialogs::snapshot_viewer::open(
        &window,
        &ctx.store,
        &ctx.services,
        snap.clone(),
        thumb,
        on_deleted,
    );
}

fn card(ctx: &Rc<Browse>, snap: &Snapshot, total: Rc<Cell<i64>>) -> gtk::Widget {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("card");
    card.set_size_request(260, -1);
    card.set_overflow(gtk::Overflow::Hidden);

    let frame = gtk::AspectFrame::new(0.5, 0.5, 16.0 / 9.0, false);
    frame.add_css_class("thumb16");
    frame.set_cursor_from_name(Some("zoom-in"));
    let picture = gtk::Picture::new();
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Cover);
    picture.set_hexpand(true);
    frame.set_child(Some(&picture));
    card.append(&frame);
    load_texture(ctx, snap, picture.clone());

    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    footer.add_css_class("footer");
    let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
    texts.set_valign(gtk::Align::Center);
    let cam = label(&snap.camera_name, &["camera"]);
    cam.set_ellipsize(gtk::pango::EllipsizeMode::End);
    texts.append(&cam);
    texts.append(&label(&format_local(snap.create_time), &["time"]));
    footer.append(&texts);
    footer.append(&spacer());

    // Removing the card: shared by the Delete button and the viewer.
    let remove_card: Rc<dyn Fn()> = {
        let card = card.clone();
        let ctx = ctx.clone();
        let total = total.clone();
        Rc::new(move || {
            if let Some(child) = card.parent() {
                if let Some(flow) = child.parent().and_downcast::<gtk::FlowBox>() {
                    flow.remove(&child);
                } else {
                    child.set_visible(false);
                }
            }
            total.set((total.get() - 1).max(0));
            ctx.loaded(total.get());
        })
    };

    let view = icons::flat_button("open_in_full", "icon-28", "View");
    {
        let ctx = ctx.clone();
        let snap = snap.clone();
        let picture = picture.clone();
        let remove_card = remove_card.clone();
        view.connect_clicked(move |b| {
            open_viewer(&ctx, b.upcast_ref(), &snap, &picture, remove_card.clone())
        });
    }
    let download = icons::flat_button("download", "icon-28", "Download");
    {
        let ctx = ctx.clone();
        let snap = snap.clone();
        download.connect_clicked(move |_| {
            start_download(
                &ctx.store,
                &ctx.services,
                DownloadKind::Snapshot(snap.id),
                media_filename(&snap.camera_name, snap.create_time, "jpg"),
            );
        });
    }
    let delete = icons::flat_button("delete", "icon-28", "Delete");
    delete.add_css_class("delete");
    {
        let ctx = ctx.clone();
        let snap_id = snap.id;
        let remove_card = remove_card.clone();
        delete.connect_clicked(move |b| {
            let Some(client) = ctx.client() else {
                ctx.toast("Not connected");
                return;
            };
            b.set_sensitive(false);
            let fut = ctx
                .services
                .run(async move { client.delete_snapshot(snap_id, 0).await });
            let ctx = ctx.clone();
            let remove_card = remove_card.clone();
            let b = b.clone();
            glib::spawn_future_local(async move {
                match fut.await {
                    Ok(()) => {
                        remove_card();
                        ctx.toast("Snapshot deleted");
                    }
                    Err(e) => {
                        b.set_sensitive(true);
                        ctx.toast(format!("Delete failed: {e}"));
                    }
                }
            });
        });
    }
    footer.append(&view);
    footer.append(&download);
    footer.append(&delete);
    card.append(&footer);

    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_PRIMARY);
    {
        let ctx = ctx.clone();
        let snap = snap.clone();
        let picture = picture.clone();
        let frame_w = frame.clone();
        click.connect_released(move |_, _, _, _| {
            open_viewer(
                &ctx,
                frame_w.upcast_ref(),
                &snap,
                &picture,
                remove_card.clone(),
            )
        });
    }
    frame.add_controller(click);

    card.upcast()
}

pub fn build(ctx: &Rc<Browse>) -> Content {
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_vexpand(true);
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let flow = gtk::FlowBox::new();
    flow.add_css_class("snap-grid");
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_homogeneous(true);
    flow.set_min_children_per_line(1);
    flow.set_max_children_per_line(8);
    flow.set_row_spacing(12);
    flow.set_column_spacing(12);
    flow.set_valign(gtk::Align::Start);
    let status = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.append(&flow);
    outer.append(&status);
    scroller.set_child(Some(&outer));

    let total = Rc::new(Cell::new(0i64));

    let load: Rc<dyn Fn()> = {
        let ctx = ctx.clone();
        let flow = flow.clone();
        let status = status.clone();
        let total = total.clone();
        Rc::new(move || {
            let generation = ctx.generation();
            ctx.take_force();
            flow.remove_all();
            super::clear_box(&status);
            let Some(client) = ctx.client() else {
                status.append(&hint("Not connected. Snapshots appear after login."));
                ctx.loaded(0);
                return;
            };
            status.append(&hint("Loading snapshots…"));
            let camera = ctx.filter.borrow().camera;
            let camera_name = camera.and_then(|id| ctx.camera_name(id));
            let (from, to) = match ctx.time_range_unix() {
                Some((a, b)) => (Some(a), Some(b)),
                None => (None, None),
            };
            let filter = SnapshotFilter {
                camera_id: camera,
                from_time: from,
                to_time: to,
                offset: ctx.offset(),
                limit: ctx.page_size as u32,
            };
            let fut = ctx
                .services
                .run(async move { client.list_snapshots(&filter).await });
            let ctx = ctx.clone();
            let flow = flow.clone();
            let status = status.clone();
            let total = total.clone();
            glib::spawn_future_local(async move {
                let result = fut.await;
                if !ctx.is_current(generation) {
                    return;
                }
                super::clear_box(&status);
                match result {
                    Ok(page) => {
                        // At least one NAS ignores camIdList: filter here too.
                        let items: Vec<&Snapshot> = page
                            .items
                            .iter()
                            .filter(|s| {
                                camera_name.as_deref().is_none_or(|n| {
                                    s.camera_name == n || s.camera_id == camera.unwrap_or(-1)
                                })
                            })
                            .collect();
                        if items.is_empty() {
                            status.append(&hint("No snapshots match the current filter."));
                        }
                        total.set(page.total);
                        for snap in items {
                            flow.append(&card(&ctx, snap, total.clone()));
                        }
                        ctx.loaded(page.total);
                    }
                    Err(e) => {
                        status.append(&hint(&format!("Could not load snapshots: {e}")));
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
