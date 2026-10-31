//! Licenses page: KPI cards, Add license, license table.

use std::rc::Rc;

use chrono::TimeZone;
use gtk::glib;
use gtk::prelude::*;
use ss_api::{License, LicenseInfo};

use crate::dialogs::shell;
use crate::icons;
use crate::services::Services;
use crate::state::{Change, Page, Store};
use crate::widgets::controls::{dot, icon_label_button, label, spacer};

/// Column widths after the `1fr` key column: Type / Quantity / Status / actions.
const COLS: [i32; 4] = [220, 120, 140, 80];

type Reload = Rc<dyn Fn()>;
/// Lets rows built by a load trigger the next load.
type ReloadCell = Rc<std::cell::RefCell<Option<Reload>>>;

/// First 4 and last 4 characters, as the reference shows keys.
pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return key.to_string();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// Type column: permanent keys never expire; the others say when.
pub fn license_type(l: &License) -> String {
    if l.is_migrated {
        return "Migrated".to_string();
    }
    if l.expired_date == 0 {
        return "Permanent".to_string();
    }
    match chrono::Local.timestamp_opt(l.expired_date, 0).single() {
        Some(t) => format!("Expires {}", t.format("%Y-%m-%d")),
        None => "Time-limited".to_string(),
    }
}

/// Status column: `(label, dot class)`.
pub fn license_status(l: &License) -> (&'static str, &'static str) {
    if l.is_expired {
        ("Expired", "offline")
    } else if l.expired_date == 0 {
        ("Permanent", "offline")
    } else {
        ("Activated", "online")
    }
}

/// `(installed, in use, available)` from `License.Load`.
pub fn kpis(info: &LicenseInfo) -> (i64, i64, i64) {
    let installed = info.key_total.max(0);
    let used = info.key_used.clamp(0, installed);
    (installed, used, installed - used)
}

fn kpi(text: &str, blue: bool) -> (gtk::Box, gtk::Label) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("kpi");
    card.add_css_class("kpi-card");
    card.append(&label(text, &["label"]));
    let value = label("–", &["value"]);
    if blue {
        value.add_css_class("blue");
    }
    card.append(&value);
    (card, value)
}

fn col(text: &str, width: i32, classes: &[&str]) -> gtk::Label {
    let l = label(text, classes);
    l.set_width_request(width);
    l.set_valign(gtk::Align::Center);
    l
}

fn window_of(w: &impl IsA<gtk::Widget>) -> Option<gtk::Window> {
    w.root().and_downcast::<gtk::Window>()
}

pub fn build(store: &Rc<Store>, services: &Rc<Services>) -> gtk::Widget {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 20);
    page.add_css_class("licenses");

    // --- top row: KPIs + actions
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let (installed_card, installed) = kpi("INSTALLED", false);
    let (used_card, used) = kpi("IN USE", true);
    let (avail_card, avail) = kpi("AVAILABLE", false);
    top.append(&installed_card);
    top.append(&used_card);
    top.append(&avail_card);
    top.append(&spacer());
    let add = icon_label_button("add", 16, "Add license");
    add.add_css_class("primary");
    add.set_widget_name("licenses-add");
    top.append(&add);
    page.append(&top);

    // --- table
    let table = gtk::Box::new(gtk::Orientation::Vertical, 0);
    table.add_css_class("table");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    header.add_css_class("table-header");
    let key_head = col("LICENSE KEY", -1, &[]);
    key_head.set_hexpand(true);
    header.append(&key_head);
    for (text, w) in ["TYPE", "QUANTITY", "STATUS", ""].iter().zip(COLS) {
        header.append(&col(text, w, &[]));
    }
    table.append(&header);
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
    table.append(&rows);
    let empty = label("No licenses loaded", &["table-empty"]);
    empty.set_halign(gtk::Align::Center);
    table.append(&empty);
    page.append(&table);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_child(Some(&page));

    // --- loading
    let reload: Rc<dyn Fn()> = {
        let store = store.clone();
        let services = services.clone();
        let rows = rows.clone();
        let empty = empty.clone();
        let (installed, used, avail) = (installed.clone(), used.clone(), avail.clone());
        let reload_cell: ReloadCell = Rc::new(std::cell::RefCell::new(None));
        let reload_cell2 = reload_cell.clone();
        let f: Rc<dyn Fn()> = Rc::new(move || {
            let Some(client) = services.client() else {
                return;
            };
            let fut = services.run(async move { client.load_licenses().await });
            let store = store.clone();
            let services = services.clone();
            let rows = rows.clone();
            let empty = empty.clone();
            let (installed, used, avail) = (installed.clone(), used.clone(), avail.clone());
            let reload_cell = reload_cell2.clone();
            glib::spawn_future_local(async move {
                let info = match fut.await {
                    Ok(info) => info,
                    Err(e) => {
                        store.toast(format!("Licenses: {e}"));
                        return;
                    }
                };
                let (i, u, a) = kpis(&info);
                installed.set_text(&i.to_string());
                used.set_text(&u.to_string());
                avail.set_text(&a.to_string());
                while let Some(c) = rows.first_child() {
                    rows.remove(&c);
                }
                empty.set_visible(info.licenses.is_empty());
                empty.set_text("No licenses installed");
                let reload = reload_cell.borrow().clone();
                for lic in &info.licenses {
                    rows.append(&license_row(&store, &services, lic, reload.clone()));
                }
            });
        });
        *reload_cell.borrow_mut() = Some(f.clone());
        f
    };

    {
        let store = store.clone();
        let services = services.clone();
        let reload = reload.clone();
        let page = page.clone();
        add.connect_clicked(move |_| {
            if let Some(w) = window_of(&page) {
                add_dialog(&w, &store, &services, reload.clone());
            }
        });
    }

    {
        let store_w = Rc::downgrade(store);
        let reload = reload.clone();
        let visible = move |store: &Store| {
            let s = store.get();
            s.page == Page::Licenses && s.logged_in()
        };
        if visible(store) {
            reload();
        }
        store.subscribe(move |c| {
            let Some(store) = store_w.upgrade() else {
                return;
            };
            match c {
                Change::Page | Change::Connection if visible(&store) => reload(),
                Change::Connection => {
                    // logged out: forget what we showed
                    while let Some(c) = rows.first_child() {
                        rows.remove(&c);
                    }
                    for l in [&installed, &used, &avail] {
                        l.set_text("–");
                    }
                    empty.set_text("No licenses loaded");
                    empty.set_visible(true);
                }
                _ => {}
            }
        });
    }

    scroller.upcast()
}

fn license_row(
    store: &Rc<Store>,
    services: &Rc<Services>,
    lic: &License,
    reload: Option<Rc<dyn Fn()>>,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("table-row");
    let key = col(&mask_key(&lic.key), -1, &["key"]);
    key.set_hexpand(true);
    key.set_selectable(false);
    row.append(&key);
    row.append(&col(&license_type(lic), COLS[0], &[]));
    row.append(&col(&lic.quota.to_string(), COLS[1], &["mono", "t12"]));
    let (status_text, dot_class) = license_status(lic);
    let status = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    status.set_width_request(COLS[2]);
    status.append(&dot(&["status-dot", "sm", dot_class]));
    status.append(&label(status_text, &[]));
    row.append(&status);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    actions.set_width_request(COLS[3]);
    let delete = icons::flat_button("delete", "icon-30", "Delete license");
    delete.add_css_class("danger");
    {
        let store = store.clone();
        let services = services.clone();
        let id = lic.id;
        let masked = mask_key(&lic.key);
        let row = row.clone();
        delete.connect_clicked(move |_| {
            if let Some(w) = window_of(&row) {
                delete_dialog(&w, &store, &services, id, &masked, reload.clone());
            }
        });
    }
    actions.append(&delete);
    row.append(&actions);
    row
}

fn add_dialog(
    parent: &gtk::Window,
    store: &Rc<Store>,
    services: &Rc<Services>,
    reload: Rc<dyn Fn()>,
) {
    let dialog = shell::present(parent, 400, "key", "Add license", None);
    let entry = gtk::Entry::new();
    entry.add_css_class("mono");
    entry.set_placeholder_text(Some("XXXX-XXXX-XXXX-XXXX"));
    let field = gtk::Box::new(gtk::Orientation::Vertical, 4);
    field.append(&label("License key", &["field-label"]));
    field.append(&entry);
    dialog.body.append(&field);
    let hint = label(
        "The NAS activates the key online with Synology's license server.",
        &["t12", "muted"],
    );
    hint.set_wrap(true);
    dialog.body.append(&hint);
    let error = label("", &["error-text"]);
    error.set_wrap(true);
    error.set_visible(false);
    dialog.body.append(&error);

    let cancel = gtk::Button::with_label("Cancel");
    {
        let window = dialog.window.clone();
        cancel.connect_clicked(move |_| window.close());
    }
    let add = gtk::Button::with_label("Add license");
    add.add_css_class("primary");
    {
        let store = store.clone();
        let services = services.clone();
        let window = dialog.window.clone();
        let entry = entry.clone();
        add.connect_clicked(move |btn| {
            let key = entry.text().trim().to_string();
            if key.is_empty() {
                error.set_text("Enter a license key.");
                error.set_visible(true);
                return;
            }
            let Some(client) = services.client() else {
                error.set_text("Not connected.");
                error.set_visible(true);
                return;
            };
            btn.set_sensitive(false);
            let fut = services.run(async move { client.add_licenses(&[key.as_str()]).await });
            let store = store.clone();
            let window = window.clone();
            let error = error.clone();
            let btn = btn.clone();
            let reload = reload.clone();
            glib::spawn_future_local(async move {
                btn.set_sensitive(true);
                match fut.await {
                    Ok(()) => {
                        store.toast("License added");
                        reload();
                        window.close();
                    }
                    Err(e) => {
                        error.set_text(&e.to_string());
                        error.set_visible(true);
                    }
                }
            });
        });
    }
    {
        let add = add.clone();
        entry.connect_activate(move |_| add.emit_clicked());
    }
    dialog.set_footer(&[&cancel, &add]);
    entry.grab_focus();
}

fn delete_dialog(
    parent: &gtk::Window,
    store: &Rc<Store>,
    services: &Rc<Services>,
    id: i64,
    masked: &str,
    reload: Option<Rc<dyn Fn()>>,
) {
    let dialog = shell::present(parent, 380, "delete", "Delete license", Some(masked));
    let text = label(
        "The key is removed from this NAS. Cameras using it are disabled until another license covers them.",
        &["t12"],
    );
    text.set_wrap(true);
    dialog.body.append(&text);
    let cancel = gtk::Button::with_label("Cancel");
    {
        let window = dialog.window.clone();
        cancel.connect_clicked(move |_| window.close());
    }
    let delete = gtk::Button::with_label("Delete");
    delete.add_css_class("destructive");
    {
        let store = store.clone();
        let services = services.clone();
        let window = dialog.window.clone();
        delete.connect_clicked(move |btn| {
            let Some(client) = services.client() else {
                store.toast("Not connected");
                return;
            };
            btn.set_sensitive(false);
            let fut = services.run(async move { client.delete_licenses(&[id]).await });
            let store = store.clone();
            let window = window.clone();
            let reload = reload.clone();
            glib::spawn_future_local(async move {
                match fut.await {
                    Ok(()) => {
                        store.toast("License deleted");
                        if let Some(r) = reload {
                            r();
                        }
                    }
                    Err(e) => store.toast(format!("Delete failed: {e}")),
                }
                window.close();
            });
        });
    }
    dialog.set_footer(&[&cancel, &delete]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masking_matches_the_reference() {
        assert_eq!(mask_key("ABCD1234EFGH5678"), "ABCD…5678");
        assert_eq!(mask_key("SHORT"), "SHORT");
        assert_eq!(mask_key("12345678"), "12345678");
    }

    #[test]
    fn status_and_type() {
        let perm = License {
            expired_date: 0,
            ..License::default()
        };
        assert_eq!(license_status(&perm), ("Permanent", "offline"));
        assert_eq!(license_type(&perm), "Permanent");
        let active = License {
            expired_date: 4_102_444_800,
            ..License::default()
        };
        assert_eq!(license_status(&active), ("Activated", "online"));
        assert!(license_type(&active).starts_with("Expires 2"));
        let expired = License {
            expired_date: 1,
            is_expired: true,
            ..License::default()
        };
        assert_eq!(license_status(&expired).0, "Expired");
    }

    #[test]
    fn kpis_clamp() {
        let info = LicenseInfo {
            key_total: 10,
            key_used: 6,
            ..LicenseInfo::default()
        };
        assert_eq!(kpis(&info), (10, 6, 4));
        let odd = LicenseInfo {
            key_total: 2,
            key_used: 5,
            ..LicenseInfo::default()
        };
        assert_eq!(kpis(&odd), (2, 2, 0));
    }
}
