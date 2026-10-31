//! About page: tile, version line, update notice, info rows, disclaimer.

use std::rc::Rc;

use gtk::prelude::*;

use crate::icons;
use crate::pages::settings::Clamp;
use crate::services::Services;
use crate::state::{Change, Store};
use crate::widgets::controls::{label, spacer};

/// `repository` from Cargo.toml; empty when the manifest has none.
const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// `v0.1.0 · GTK4 · GStreamer 1.24.5`
pub fn version_line() -> String {
    let (maj, min, micro, _) = gstreamer::version();
    format!(
        "v{} · GTK4 · GStreamer {maj}.{min}.{micro}",
        env!("CARGO_PKG_VERSION")
    )
}

/// The Connected NAS row.
pub fn connection_text(c: Option<&crate::state::Connection>) -> String {
    match c {
        None => "Not connected".to_string(),
        Some(c) => {
            let mut s = format!("{} · {}", c.profile, c.host);
            if !c.ds_model.is_empty() {
                s.push_str(&format!(" · {}", c.ds_model));
            }
            if !c.ss_version.is_empty() {
                s.push_str(&format!(" · SS {}", c.ss_version));
            }
            s
        }
    }
}

fn info_row(key: &str, value: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("info-row");
    let k = label(key, &["key"]);
    k.set_valign(gtk::Align::Center);
    row.append(&k);
    value.set_valign(gtk::Align::Center);
    value.set_hexpand(true);
    row.append(value);
    row
}

pub fn build(store: &Rc<Store>, _services: &Rc<Services>) -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 16);
    column.add_css_class("about");
    column.set_valign(gtk::Align::Start);

    let tile = gtk::Label::new(Some("Sy"));
    tile.add_css_class("tile");
    tile.set_size_request(64, 64);
    tile.set_halign(gtk::Align::Center);
    column.append(&tile);

    let title = label("Synoptic", &["title"]);
    title.set_halign(gtk::Align::Center);
    column.append(&title);
    let version = label(&version_line(), &["version"]);
    version.set_halign(gtk::Align::Center);
    version.set_selectable(true);
    column.append(&version);

    // update notice
    let notice = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    notice.add_css_class("notice");
    let icon = icons::image("new_releases", 18);
    icon.set_valign(gtk::Align::Start);
    notice.append(&icon);
    let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
    texts.append(&label("A newer release is available", &["title"]));
    if REPO_URL.is_empty() {
        texts.append(&label(
            "See the project's releases page for notes.",
            &["t12", "muted"],
        ));
    } else {
        let link = gtk::LinkButton::with_label(&format!("{REPO_URL}/releases"), "Release notes");
        link.set_halign(gtk::Align::Start);
        texts.append(&link);
    }
    notice.append(&texts);
    notice.append(&spacer());
    let dismiss = icons::flat_button("close", "icon-28", "Dismiss");
    dismiss.set_valign(gtk::Align::Start);
    {
        let store = store.clone();
        dismiss.connect_clicked(move |_| {
            store.update(Change::UpdateNotice, |s| s.update_notice_unread = false);
        });
    }
    notice.append(&dismiss);
    notice.set_visible(store.get().update_notice_unread);
    column.append(&notice);

    // info rows
    let info = gtk::Box::new(gtk::Orientation::Vertical, 0);
    info.append(&info_row("License", &label("BSD 2-Clause", &["val"])));
    let repo: gtk::Widget = if REPO_URL.is_empty() {
        label("—", &["val", "muted"]).upcast()
    } else {
        let short = REPO_URL
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        let link = gtk::LinkButton::with_label(REPO_URL, short);
        link.set_halign(gtk::Align::Start);
        link.upcast()
    };
    info.append(&info_row("Repository", &repo));
    let nas = label(&connection_text(store.get().connection.as_ref()), &["val"]);
    nas.set_ellipsize(gtk::pango::EllipsizeMode::End);
    info.append(&info_row("Connected NAS", &nas));
    let config_path = ss_config::Paths::from_env()
        .map(|p| p.config_file().display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let path = label(&config_path, &["val", "mono", "t12"]);
    path.set_selectable(true);
    path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    info.append(&info_row("Config path", &path));
    column.append(&info);

    let disclaimer = label(
        "Not affiliated with or endorsed by Synology Inc. Synology, DiskStation and Surveillance Station are trademarks of Synology Inc. Protocol knowledge is ported from the BSD-2 reference client credited in NOTICE.",
        &["disclaimer"],
    );
    disclaimer.set_wrap(true);
    disclaimer.set_justify(gtk::Justification::Center);
    disclaimer.set_halign(gtk::Align::Center);
    disclaimer.set_xalign(0.5);
    column.append(&disclaimer);

    {
        let store_w = Rc::downgrade(store);
        store.subscribe(move |c| {
            let Some(store) = store_w.upgrade() else {
                return;
            };
            match c {
                Change::UpdateNotice => notice.set_visible(store.get().update_notice_unread),
                Change::Connection => {
                    nas.set_text(&connection_text(store.get().connection.as_ref()));
                }
                _ => {}
            }
        });
    }

    let clamp = Clamp::new(520, &column);
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_child(Some(&clamp));
    scroller.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Connection;

    #[test]
    fn connection_row() {
        assert_eq!(connection_text(None), "Not connected");
        let c = Connection {
            profile: "home-nas".into(),
            host: "192.168.1.100".into(),
            username: "ra".into(),
            ss_version: "9.3.1-11xxx".into(),
            ds_model: "DS923+".into(),
        };
        assert_eq!(
            connection_text(Some(&c)),
            "home-nas · 192.168.1.100 · DS923+ · SS 9.3.1-11xxx"
        );
    }
}
