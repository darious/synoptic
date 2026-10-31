//! 240 px collapsible camera panel.

use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use ss_api::CameraStatus;

use crate::icons;
use crate::state::{Change, Store};
use crate::widgets::controls::{divider_h, dot, ellipsized, label, set_class, spacer, tag};

pub fn build(store: &Rc<Store>) -> gtk::Revealer {
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideRight);
    revealer.set_transition_duration(200);
    revealer.set_reveal_child(store.get().panel_visible);

    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("camera-panel");
    panel.set_width_request(240);

    // header: CAMERAS  5/6            hint
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    head.add_css_class("panel-header");
    head.set_margin_start(8);
    head.set_margin_end(8);
    head.append(&label("CAMERAS", &["section-header"]));
    let count = label("0/0", &["mono", "t11", "muted"]);
    head.append(&count);
    head.append(&spacer());
    let hint = label("", &["hint"]);
    head.append(&hint);
    panel.append(&head);

    // list
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_vexpand(true);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    scroller.set_child(Some(&list));
    panel.append(&scroller);

    // footer: DSM · SS version          clock
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    footer.add_css_class("panel-footer");
    let version = label("", &["t11", "muted"]);
    footer.append(&version);
    footer.append(&spacer());
    let clock = label("", &["mono", "t11", "muted"]);
    footer.append(&clock);
    panel.append(&footer);
    {
        let clock = clock.clone();
        let tick = move || clock.set_text(&chrono::Local::now().format("%H:%M:%S").to_string());
        tick();
        glib::timeout_add_seconds_local(1, move || {
            tick();
            glib::ControlFlow::Continue
        });
    }

    revealer.set_child(Some(&panel));

    let rebuild = {
        let store = store.clone();
        let list = list.clone();
        move || {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            let s = store.get();
            set_class(&list, "compact", s.density_compact);

            if !s.logged_in() {
                let empty = gtk::Box::new(gtk::Orientation::Vertical, 8);
                empty.set_valign(gtk::Align::Center);
                empty.set_vexpand(true);
                empty.set_margin_top(40);
                let icon = icons::image("cloud_off", 28);
                icon.add_css_class("muted");
                empty.append(&icon);
                let text = label(
                    "Not connected.\nCameras appear after login.",
                    &["muted", "t12"],
                );
                text.set_justify(gtk::Justification::Center);
                text.set_xalign(0.5);
                empty.append(&text);
                list.append(&empty);
                count.set_text("0/0");
                hint.set_text("");
                version.set_text("");
                return;
            }

            count.set_text(&format!("{}/{}", s.online_count(), s.cameras.len()));
            hint.set_text(&match (s.page.is_browse(), s.selected_slot) {
                (true, _) => "click to filter".to_string(),
                (false, Some(i)) => format!("Slot {} selected", i + 1),
                _ => String::new(),
            });
            if let Some(c) = &s.connection {
                version.set_text(&format!("{} · SS {}", c.ds_model, c.ss_version));
            }

            if s.page.is_browse() {
                let all = row_button("select_all", "All cameras");
                set_class(&all, "active-filter", s.filter_camera.is_none());
                let store2 = store.clone();
                all.connect_clicked(move |_| {
                    store2.update(Change::FilterCamera, |s| s.filter_camera = None);
                });
                list.append(&all);
            }

            for cam in &s.cameras {
                let online = cam.status == CameraStatus::Enabled;
                let row = gtk::Button::new();
                row.add_css_class("camera-row");
                set_class(&row, "offline", !online);
                let in_layout = s.slot_of(cam.id);
                set_class(
                    &row,
                    "in-layout",
                    !s.page.is_browse() && in_layout.is_some(),
                );
                set_class(
                    &row,
                    "active-filter",
                    s.page.is_browse() && s.filter_camera == Some(cam.id),
                );

                let h = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                h.append(&dot(&[
                    "status-dot",
                    if online { "online" } else { "offline" },
                ]));
                let names = gtk::Box::new(gtk::Orientation::Vertical, 1);
                names.set_hexpand(true);
                names.set_valign(gtk::Align::Center);
                names.append(&ellipsized(&cam.name, &["name"]));
                names.append(&ellipsized(&cam.model, &["model"]));
                h.append(&names);
                if cam.is_ptz {
                    h.append(&tag("PTZ"));
                }
                if let Some(i) = in_layout {
                    let b = gtk::Label::new(Some(&(i + 1).to_string()));
                    b.add_css_class("slot-badge");
                    b.set_valign(gtk::Align::Center);
                    h.append(&b);
                }
                let tune = icons::flat_button("tune", "icon-24", "Camera settings");
                {
                    let store2 = store.clone();
                    let id = cam.id;
                    tune.connect_clicked(move |_| store2.emit(&Change::RequestCameraSettings(id)));
                }
                h.append(&tune);
                row.set_child(Some(&h));

                {
                    let store2 = store.clone();
                    let id = cam.id;
                    row.connect_clicked(move |_| on_camera_clicked(&store2, id));
                }
                {
                    let store2 = store.clone();
                    let id = cam.id;
                    let gesture = gtk::GestureClick::new();
                    gesture.set_button(3);
                    gesture.connect_pressed(move |_, _, _, _| {
                        store2.emit(&Change::RequestCameraSettings(id));
                    });
                    row.add_controller(gesture);
                }
                list.append(&row);
            }

            if !s.page.is_browse() {
                list.append(&divider_h());
                let empty_slot = row_button("crop_square", "Empty slot");
                empty_slot.add_css_class("muted");
                let store2 = store.clone();
                empty_slot.connect_clicked(move |_| on_camera_clicked(&store2, 0));
                list.append(&empty_slot);
            }
        }
    };
    rebuild();

    {
        let revealer = revealer.clone();
        let store2 = store.clone();
        store.subscribe(move |change| match change {
            Change::PanelVisible => revealer.set_reveal_child(store2.get().panel_visible),
            Change::Connection
            | Change::Cameras
            | Change::Page
            | Change::Layout
            | Change::Slots
            | Change::SelectedSlot
            | Change::FilterCamera
            | Change::Density => rebuild(),
            _ => {}
        });
    }

    revealer
}

fn row_button(material: &str, text: &str) -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("camera-row");
    b.add_css_class("row-36");
    let h = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    h.append(&icons::image(material, 16));
    h.append(&label(text, &[]));
    b.set_child(Some(&h));
    b
}

/// Live View: put the camera (0 = empty) into the selected slot, or the first
/// free slot. Browse pages: set the camera filter.
fn on_camera_clicked(store: &Rc<Store>, camera_id: i64) {
    let (is_browse, selected, slots, layout) = {
        let s = store.get();
        (s.page.is_browse(), s.selected_slot, s.slots(), s.layout)
    };
    if is_browse {
        store.update(Change::FilterCamera, |s| s.filter_camera = Some(camera_id));
        return;
    }
    let target = selected.or_else(|| slots.iter().position(|&c| c == 0));
    let Some(target) = target else {
        store.toast("Layout is full — select a slot first");
        return;
    };
    store.update(Change::Slots, |s| {
        let mut v = slots.clone();
        // a camera lives in one slot at a time
        if camera_id != 0 {
            for c in v.iter_mut() {
                if *c == camera_id {
                    *c = 0;
                }
            }
        }
        v[target] = camera_id;
        s.slots_by_layout.insert(layout, v);
        s.selected_slot = None;
    });
    store.emit(&Change::SelectedSlot);
}
