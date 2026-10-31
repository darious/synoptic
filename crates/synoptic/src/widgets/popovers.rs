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
use crate::state::{Change, Layout, Store};
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

/// Layout menu (200 w): 1x1 … 4x4 with `filled/total`, divider, Clear layout.
pub fn layout_menu(store: &Rc<Store>) -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.set_width_request(200);

    let mut rows = Vec::new();
    for layout in Layout::ALL {
        let row = menu_row(layout.icon(), layout.key(), Some("0/0"));
        {
            let store = store.clone();
            let popover = popover.clone();
            row.connect_clicked(move |_| {
                store.update(Change::Layout, |s| {
                    s.layout = layout;
                    s.selected_slot = None;
                });
                popover.popdown();
            });
        }
        content.append(&row);
        rows.push((layout, row));
    }
    content.append(&divider_h());
    let clear = menu_row("delete_sweep", "Clear layout", None);
    clear.add_css_class("danger");
    {
        let store = store.clone();
        let popover = popover.clone();
        clear.connect_clicked(move |_| {
            store.update(Change::Slots, |s| {
                let n = s.layout.slots();
                s.slots_by_layout.insert(s.layout, vec![0; n]);
                s.selected_slot = None;
            });
            store.toast("Layout cleared");
            popover.popdown();
        });
    }
    content.append(&clear);
    popover.set_child(Some(&content));

    let refresh = {
        let store = store.clone();
        move || {
            let s = store.get();
            for (layout, row) in &rows {
                set_class(row, "current", s.layout == *layout);
                let slots = s
                    .slots_by_layout
                    .get(layout)
                    .map(|v| v.iter().filter(|&&c| c != 0).count())
                    .unwrap_or(0);
                if let Some(b) = row.child().and_then(|c| c.downcast::<gtk::Box>().ok())
                    && let Some(l) = b.last_child().and_then(|c| c.downcast::<gtk::Label>().ok())
                {
                    l.set_text(&format!("{slots}/{}", layout.slots()));
                }
            }
        }
    };
    refresh();
    store.subscribe(move |c| {
        if matches!(c, Change::Layout | Change::Slots) {
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
