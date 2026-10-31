//! Popover menus: layout menu, notifications, account.
//!
//! Each builder returns a `gtk::Popover` to attach to a `gtk::MenuButton`.

use std::rc::Rc;

use gtk::prelude::*;

use crate::icons;
use crate::state::{Change, Layout, Store};
use crate::widgets::controls::{divider_h, label, set_class, spacer};

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

/// Notifications popover (340 w). Rows are owned by whoever fills
/// `Change::Notifications`; this shell shows the header and an empty state.
pub fn notifications(store: &Rc<Store>) -> gtk::Popover {
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
    {
        let store = store.clone();
        mark.connect_clicked(move |_| {
            store.update(Change::Notifications, |s| s.unread_notifications = 0);
        });
    }
    header.append(&mark);
    content.append(&header);
    content.append(&divider_h());

    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.add_css_class("notification-list");
    let empty = label("No notifications", &["muted", "t12"]);
    empty.set_margin_top(12);
    empty.set_margin_bottom(12);
    empty.set_halign(gtk::Align::Center);
    list.append(&empty);
    content.append(&list);
    popover.set_child(Some(&content));
    popover
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
