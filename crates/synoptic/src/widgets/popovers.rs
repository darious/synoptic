//! Popover menus: layout menu, notifications, account.
//!
//! Each builder returns a `gtk::Popover` to attach to a `gtk::MenuButton`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::TimeZone;
use gtk::glib;
use gtk::prelude::*;
use ss_api::Alert;

use crate::icons;
use crate::services::Services;
use crate::state::{Change, Shape, Store};
use crate::widgets::controls::{divider_h, ellipsized, label, set_class, spacer};

/// A 32 h menu row: icon, label, optional right-aligned mono text.
pub fn menu_row(material: &str, text: &str, right: Option<&str>) -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("menu-row");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&icons::image(material, 16));
    row.append(&label(text, &[]));
    if let Some(r) = right {
        row.append(&spacer());
        row.append(&label(r, &["mono", "t11", "muted"]));
    }
    b.set_child(Some(&row));
    b
}

/// What the inline name entry of the layout menu is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NameMode {
    New,
    Rename,
}

/// A 26×18 miniature of a [`Shape`]: its cells drawn with the widget's
/// foreground colour.
pub fn shape_icon(shape: Shape) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(26);
    area.set_content_height(18);
    area.set_halign(gtk::Align::Center);
    area.set_draw_func(move |w, cr, width, height| {
        let (rows, cols) = shape.dims();
        let gap = 1.5;
        let cw = (f64::from(width) - gap * (cols as f64 - 1.0)) / cols as f64;
        let ch = (f64::from(height) - gap * (rows as f64 - 1.0)) / rows as f64;
        let c = w.color();
        cr.set_source_rgba(
            f64::from(c.red()),
            f64::from(c.green()),
            f64::from(c.blue()),
            f64::from(c.alpha()),
        );
        for i in 0..shape.slots() {
            let cell = shape.cell(i);
            let x = f64::from(cell.col) * (cw + gap);
            let y = f64::from(cell.row) * (ch + gap);
            let wd = f64::from(cell.colspan) * cw + f64::from(cell.colspan - 1) * gap;
            let ht = f64::from(cell.rowspan) * ch + f64::from(cell.rowspan - 1) * gap;
            cr.rectangle(x, y, wd, ht);
        }
        let _ = cr.fill();
    });
    area
}

/// Layout menu (260 w): the profile's saved layouts (click = switch; the
/// active one highlighted with `shape · filled/total`), a shape picker that
/// reshapes the active layout, then New / Rename / Duplicate / Delete /
/// Clear slots. Widget names: `layout-use-<i>`, `layout-shape-<key>`,
/// `layout-new`, `layout-rename`, `layout-name-entry`, `layout-name-confirm`,
/// `layout-duplicate`, `layout-delete`, `layout-clear`.
pub fn layout_menu(store: &Rc<Store>) -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    popover.add_css_class("layout-menu");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.set_width_request(260);

    let section = |text: &str| {
        let l = label(text, &["section-header"]);
        l.set_margin_start(8);
        l.set_margin_top(4);
        l.set_margin_bottom(2);
        l
    };

    // saved layouts
    content.append(&section("LAYOUTS"));
    let layouts_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.append(&layouts_box);
    content.append(&divider_h());

    // shape picker: 6 × 2 miniature buttons
    content.append(&section("SHAPE"));
    let shapes = gtk::Grid::new();
    shapes.set_row_spacing(2);
    shapes.set_column_spacing(2);
    shapes.set_column_homogeneous(true);
    shapes.set_margin_start(4);
    shapes.set_margin_end(4);
    let mut shape_buttons = Vec::new();
    for (i, shape) in Shape::ALL.into_iter().enumerate() {
        let b = gtk::Button::new();
        b.add_css_class("shape");
        b.set_widget_name(&format!("layout-shape-{}", shape.key()));
        b.set_tooltip_text(Some(&format!("{} slots", shape.slots())));
        let v = gtk::Box::new(gtk::Orientation::Vertical, 3);
        v.append(&shape_icon(shape));
        let k = label(shape.key(), &["mono", "t11"]);
        k.set_xalign(0.5);
        v.append(&k);
        b.set_child(Some(&v));
        {
            let store = store.clone();
            b.connect_clicked(move |_| {
                store.update(Change::Layout, |s| s.set_shape(shape));
                store.emit(&Change::Slots);
                store.emit(&Change::SelectedSlot);
            });
        }
        shapes.attach(&b, (i % 6) as i32, (i / 6) as i32, 1, 1);
        shape_buttons.push((shape, b));
    }
    content.append(&shapes);
    content.append(&divider_h());

    // inline name entry (New / Rename), hidden until asked for
    let name_mode = Rc::new(Cell::new(NameMode::New));
    let name_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    name_row.set_margin_start(4);
    name_row.set_margin_end(4);
    name_row.set_margin_top(2);
    name_row.set_margin_bottom(2);
    name_row.set_visible(false);
    let name_entry = gtk::Entry::new();
    name_entry.set_widget_name("layout-name-entry");
    name_entry.set_hexpand(true);
    let name_confirm = gtk::Button::with_label("Create");
    name_confirm.set_widget_name("layout-name-confirm");
    name_confirm.add_css_class("primary");
    let name_cancel = icons::flat_button("close", "icon-30", "Cancel");
    name_cancel.set_widget_name("layout-name-cancel");
    name_row.append(&name_entry);
    name_row.append(&name_confirm);
    name_row.append(&name_cancel);
    content.append(&name_row);

    let confirm = {
        let store = store.clone();
        let popover = popover.clone();
        let name_row = name_row.clone();
        let name_entry = name_entry.clone();
        let name_mode = name_mode.clone();
        Rc::new(move || {
            let text = name_entry.text().to_string();
            match name_mode.get() {
                NameMode::New => {
                    let shape = store.get().shape();
                    store.update(Change::Layout, |s| {
                        s.add_layout(&text, shape, true);
                    });
                    store.emit(&Change::Slots);
                    store.emit(&Change::SelectedSlot);
                    let name = store
                        .get()
                        .layout()
                        .map(|l| l.name.clone())
                        .unwrap_or_default();
                    store.toast(format!("Layout “{name}” created"));
                    popover.popdown();
                }
                NameMode::Rename => {
                    let ok = {
                        let mut renamed = false;
                        store.update(Change::Layout, |s| renamed = s.rename_layout(&text));
                        renamed
                    };
                    if !ok {
                        store.toast("That name is empty or already used");
                        return;
                    }
                }
            }
            name_row.set_visible(false);
        })
    };
    {
        let confirm = confirm.clone();
        name_confirm.connect_clicked(move |_| confirm());
    }
    {
        let confirm = confirm.clone();
        name_entry.connect_activate(move |_| confirm());
    }
    {
        let name_row = name_row.clone();
        name_cancel.connect_clicked(move |_| name_row.set_visible(false));
    }

    let show_name_row = {
        let store = store.clone();
        let name_row = name_row.clone();
        let name_entry = name_entry.clone();
        let name_confirm = name_confirm.clone();
        let name_mode = name_mode.clone();
        move |mode: NameMode| {
            name_mode.set(mode);
            let s = store.get();
            match mode {
                NameMode::New => {
                    name_entry.set_text("");
                    name_entry.set_placeholder_text(Some(s.shape().key()));
                    name_confirm.set_label("Create");
                }
                NameMode::Rename => {
                    name_entry.set_text(&s.layout().map(|l| l.name.clone()).unwrap_or_default());
                    name_entry.set_placeholder_text(None);
                    name_confirm.set_label("Rename");
                }
            }
            drop(s);
            name_row.set_visible(true);
            name_entry.grab_focus();
        }
    };
    let show_name_row = Rc::new(show_name_row);

    // actions
    let new_row = menu_row("add", "New layout…", None);
    new_row.set_widget_name("layout-new");
    {
        let show = show_name_row.clone();
        new_row.connect_clicked(move |_| show(NameMode::New));
    }
    content.append(&new_row);
    let rename_row = menu_row("edit", "Rename…", None);
    rename_row.set_widget_name("layout-rename");
    {
        let show = show_name_row.clone();
        rename_row.connect_clicked(move |_| show(NameMode::Rename));
    }
    content.append(&rename_row);
    let dup_row = menu_row("select_all", "Duplicate", None);
    dup_row.set_widget_name("layout-duplicate");
    {
        let store = store.clone();
        let popover = popover.clone();
        dup_row.connect_clicked(move |_| {
            store.update(Change::Layout, |s| {
                s.duplicate_layout();
            });
            store.emit(&Change::Slots);
            store.emit(&Change::SelectedSlot);
            popover.popdown();
        });
    }
    content.append(&dup_row);
    let delete_row = menu_row("delete", "Delete", None);
    delete_row.set_widget_name("layout-delete");
    delete_row.add_css_class("danger");
    {
        let store = store.clone();
        let popover = popover.clone();
        delete_row.connect_clicked(move |_| {
            let name = store
                .get()
                .layout()
                .map(|l| l.name.clone())
                .unwrap_or_default();
            let mut deleted = false;
            store.update(Change::Layout, |s| deleted = s.delete_layout());
            if deleted {
                store.emit(&Change::Slots);
                store.emit(&Change::SelectedSlot);
                store.toast(format!("Layout “{name}” deleted"));
                popover.popdown();
            } else {
                store.toast("The last layout cannot be deleted");
            }
        });
    }
    content.append(&delete_row);
    let clear = menu_row("delete_sweep", "Clear slots", None);
    clear.set_widget_name("layout-clear");
    {
        let store = store.clone();
        let popover = popover.clone();
        clear.connect_clicked(move |_| {
            store.update(Change::Slots, |s| s.clear_slots());
            store.emit(&Change::SelectedSlot);
            store.toast("Slots cleared");
            popover.popdown();
        });
    }
    content.append(&clear);
    popover.set_child(Some(&content));

    // hide the name entry whenever the menu closes
    {
        let name_row = name_row.clone();
        popover.connect_closed(move |_| name_row.set_visible(false));
    }

    let refresh = {
        let store = store.clone();
        let popover = popover.clone();
        move || {
            let s = store.get();
            while let Some(c) = layouts_box.first_child() {
                layouts_box.remove(&c);
            }
            let shape = s.shape();
            for (i, l) in s.layouts.iter().enumerate() {
                let row = menu_row(
                    l.shape.icon(),
                    &l.name,
                    Some(&format!(
                        "{} · {}/{}",
                        l.shape.key(),
                        l.filled(),
                        l.shape.slots()
                    )),
                );
                row.set_widget_name(&format!("layout-use-{i}"));
                set_class(&row, "current", i == s.active_layout);
                let store = store.clone();
                let popover = popover.clone();
                row.connect_clicked(move |_| {
                    store.update(Change::Layout, |s| s.activate_layout(i));
                    store.emit(&Change::Slots);
                    store.emit(&Change::SelectedSlot);
                    popover.popdown();
                });
                layouts_box.append(&row);
            }
            if s.layouts.is_empty() {
                let none = label("No layouts yet — log in", &["muted", "t12"]);
                none.set_margin_start(8);
                layouts_box.append(&none);
            }
            for (sh, b) in &shape_buttons {
                set_class(b, "current", *sh == shape);
            }
            delete_row.set_sensitive(s.layouts.len() > 1);
            let has = !s.layouts.is_empty();
            rename_row.set_sensitive(has);
            dup_row.set_sensitive(has);
        }
    };
    refresh();
    store.subscribe(move |c| {
        if matches!(c, Change::Layout | Change::Slots | Change::Connection) {
            refresh();
        }
    });
    popover
}

// --- notifications -------------------------------------------------------------

thread_local! {
    /// Services handle for the popover's closures; set by [`notifications`].
    static SERVICES: RefCell<Option<Rc<Services>>> = const { RefCell::new(None) };
}

fn services() -> Option<Rc<Services>> {
    SERVICES.with(|s| s.borrow().clone())
}

/// Newest alerts the popover shows (the reference fetches 20 too).
const ALERT_LIMIT: u32 = 20;

/// `(icon, colour class, text)` for a Surveillance Station alert type.
pub fn alert_kind(alert_type: i64) -> (&'static str, &'static str, &'static str) {
    match alert_type {
        0 => ("directions_run", "type-motion", "Motion detected"),
        1 => ("wifi_off", "type-tampering", "Connection lost"),
        2 => ("check_circle", "type-people", "Connection restored"),
        3 => ("warning", "type-motion", "Alarm triggered"),
        4 => ("videocam_off", "muted", "Camera disabled"),
        5 => ("videocam", "type-people", "Camera enabled"),
        _ => ("notifications", "type-vehicle", "Alert"),
    }
}

/// `HH:MM` today, `MM-DD HH:MM` otherwise.
pub fn alert_time(ts: i64, now: chrono::DateTime<chrono::Local>) -> String {
    match chrono::Local.timestamp_opt(ts, 0).single() {
        Some(t) if t.date_naive() == now.date_naive() => t.format("%H:%M").to_string(),
        Some(t) => t.format("%m-%d %H:%M").to_string(),
        None => "–".to_string(),
    }
}

fn alert_row(a: &Alert) -> gtk::Box {
    let (icon, class, text) = alert_kind(a.alert_type);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("notification-row");
    set_class(&row, "unread", !a.is_read);
    let img = icons::image(icon, 18);
    img.add_css_class(class);
    img.set_valign(gtk::Align::Center);
    row.append(&img);
    let what = if a.camera_name.is_empty() {
        text.to_string()
    } else {
        format!("{text} · {}", a.camera_name)
    };
    row.append(&ellipsized(&what, &["text"]));
    let time = label(&alert_time(a.timestamp, chrono::Local::now()), &["time"]);
    time.set_valign(gtk::Align::Center);
    row.append(&time);
    row
}

/// Notifications popover (340 w): rows from `Notification.List`, "Mark
/// these read", and the unread-count poll that feeds the header badge.
pub fn notifications(store: &Rc<Store>, svc: &Rc<Services>) -> gtk::Popover {
    SERVICES.with(|s| *s.borrow_mut() = Some(svc.clone()));
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_width_request(340);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.set_height_request(38);
    header.append(&label("Notifications", &["w600"]));
    header.append(&spacer());
    let mark = gtk::Button::with_label("Mark these read");
    mark.set_height_request(26);
    mark.add_css_class("t11");
    mark.set_valign(gtk::Align::Center);
    header.append(&mark);
    content.append(&header);
    content.append(&divider_h());

    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.add_css_class("notification-list");
    let empty = label("No notifications", &["muted", "t12"]);
    empty.set_margin_top(12);
    empty.set_margin_bottom(12);
    empty.set_halign(gtk::Align::Center);
    content.append(&list);
    content.append(&empty);
    popover.set_child(Some(&content));

    // Alerts currently shown (ids + read state), for "Mark these read".
    let shown: Rc<RefCell<Vec<Alert>>> = Rc::new(RefCell::new(Vec::new()));

    let render = {
        let list = list.clone();
        let empty = empty.clone();
        move |alerts: &[Alert]| {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            for a in alerts {
                list.append(&alert_row(a));
            }
            empty.set_visible(alerts.is_empty());
        }
    };
    let render = Rc::new(render);

    // fetch when opened
    {
        let store = store.clone();
        let shown = shown.clone();
        let render = render.clone();
        let mark = mark.clone();
        popover.connect_visible_notify(move |p| {
            if !p.is_visible() {
                return;
            }
            let Some(services) = services() else { return };
            let Some(client) = services.client() else {
                return;
            };
            mark.set_sensitive(false);
            let fut = services.run(async move { client.list_alerts(0, ALERT_LIMIT).await });
            let store = store.clone();
            let shown = shown.clone();
            let render = render.clone();
            let mark = mark.clone();
            glib::spawn_future_local(async move {
                mark.set_sensitive(true);
                match fut.await {
                    Ok(page) => {
                        render(&page.items);
                        *shown.borrow_mut() = page.items;
                    }
                    Err(e) => store.toast(format!("Notifications: {e}")),
                }
            });
        });
    }

    // Mark these read
    {
        let store = store.clone();
        let shown = shown.clone();
        let render = render.clone();
        mark.connect_clicked(move |_| {
            let ids: Vec<i64> = shown
                .borrow()
                .iter()
                .filter(|a| !a.is_read)
                .map(|a| a.id)
                .collect();
            let store = store.clone();
            let shown = shown.clone();
            let render = render.clone();
            let finish = move || {
                for a in shown.borrow_mut().iter_mut() {
                    a.is_read = true;
                }
                render(&shown.borrow());
                store.update(Change::Notifications, |s| s.unread_notifications = 0);
            };
            let client = services().and_then(|s| s.client());
            match (client, ids.is_empty()) {
                (Some(client), false) => {
                    let Some(services) = services() else { return };
                    let fut = services.run(async move { client.mark_alerts_read(&ids).await });
                    glib::spawn_future_local(async move {
                        if let Err(e) = fut.await {
                            tracing::warn!("mark alerts read: {e}");
                        }
                        finish();
                    });
                }
                _ => finish(),
            }
        });
    }

    // unread-count polling while logged in
    let generation = Rc::new(Cell::new(0u64));
    {
        let store_w = Rc::downgrade(store);
        let generation = generation.clone();
        let shown = shown.clone();
        let render = render.clone();
        store.subscribe(move |c| {
            if !matches!(c, Change::Connection) {
                return;
            }
            let Some(store) = store_w.upgrade() else {
                return;
            };
            // stop any running poller; a fresh one starts below when logged in
            let g = generation.get() + 1;
            generation.set(g);
            if !store.get().logged_in() {
                shown.borrow_mut().clear();
                render(&[]);
                return;
            }
            let Some(services) = services() else { return };
            let secs = store.config().general.poll_interval_alerts.max(5);
            poll_unread(&store, &services);
            let store_w = Rc::downgrade(&store);
            let generation = generation.clone();
            glib::timeout_add_seconds_local(secs, move || {
                let Some(store) = store_w.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if generation.get() != g || !store.get().logged_in() {
                    return glib::ControlFlow::Break;
                }
                poll_unread(&store, &services);
                glib::ControlFlow::Continue
            });
        });
    }

    popover
}

fn poll_unread(store: &Rc<Store>, services: &Rc<Services>) {
    let Some(client) = services.client() else {
        return;
    };
    let fut = services.run(async move { client.count_unread_alerts().await });
    let store = store.clone();
    glib::spawn_future_local(async move {
        match fut.await {
            Ok(n) => {
                let n = u32::try_from(n.max(0)).unwrap_or(u32::MAX);
                if store.get().unread_notifications != n {
                    store.update(Change::Notifications, |s| s.unread_notifications = n);
                }
            }
            Err(e) => tracing::debug!("unread alerts: {e}"),
        }
    });
}

/// Account menu: user, host, Sign out.
pub fn account(store: &Rc<Store>) -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.set_width_request(220);
    let user = label("", &["w600"]);
    let host = label("", &["muted", "t12"]);
    user.set_margin_start(8);
    host.set_margin_start(8);
    host.set_margin_bottom(4);
    content.append(&user);
    content.append(&host);
    content.append(&divider_h());
    let sign_out = menu_row("logout", "Sign out", None);
    {
        let store = store.clone();
        let popover = popover.clone();
        sign_out.connect_clicked(move |_| {
            popover.popdown();
            store.emit(&Change::RequestLogout);
        });
    }
    content.append(&sign_out);
    popover.set_child(Some(&content));

    let refresh = {
        let store = store.clone();
        move || {
            let s = store.get();
            if let Some(c) = &s.connection {
                user.set_text(&c.username);
                host.set_text(&format!("{} · {}", c.profile, c.host));
            }
        }
    };
    refresh();
    store.subscribe(move |c| {
        if matches!(c, Change::Connection) {
            refresh();
        }
    });
    popover
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Local, TimeZone};

    #[test]
    fn alert_types_follow_the_reference_table() {
        assert_eq!(alert_kind(0).2, "Motion detected");
        assert_eq!(alert_kind(1).2, "Connection lost");
        assert_eq!(alert_kind(5).2, "Camera enabled");
        assert_eq!(alert_kind(99).2, "Alert");
    }

    #[test]
    fn times_shorten_for_today() {
        let now = Local.with_ymd_and_hms(2026, 10, 31, 15, 0, 0).unwrap();
        let today = now - Duration::hours(2);
        assert_eq!(alert_time(today.timestamp(), now), "13:00");
        let yesterday = now - Duration::days(1);
        assert_eq!(alert_time(yesterday.timestamp(), now), "10-30 15:00");
    }
}
