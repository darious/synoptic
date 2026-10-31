//! Small reusable builders: labels with classes, dividers, dots, pills.

use gtk::prelude::*;

/// A label carrying CSS classes (`"mono"`, `"t11"`, `"muted"`, …).
pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    for c in classes {
        l.add_css_class(c);
    }
    l.set_xalign(0.0);
    l
}

/// Ellipsizing label.
pub fn ellipsized(text: &str, classes: &[&str]) -> gtk::Label {
    let l = label(text, classes);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    l.set_hexpand(true);
    l
}

/// 1×22 vertical divider.
pub fn divider_v() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.add_css_class("divider-v");
    b.set_valign(gtk::Align::Center);
    b
}

/// Full-width 1 px horizontal divider.
pub fn divider_h() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    b.add_css_class("divider-h");
    b
}

/// Expanding spacer.
pub fn spacer() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    b.set_hexpand(true);
    b
}

/// Coloured dot: classes such as `status-dot online`.
pub fn dot(classes: &[&str]) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    for c in classes {
        b.add_css_class(c);
    }
    b.set_valign(gtk::Align::Center);
    b
}

/// Mono tag such as `PTZ`.
pub fn tag(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("tag");
    l.set_valign(gtk::Align::Center);
    l
}

/// Filter pill chip (28 h, radius 14). Toggle `.active` yourself.
pub fn chip(text: &str) -> gtk::Button {
    let b = gtk::Button::with_label(text);
    b.add_css_class("chip");
    b.set_valign(gtk::Align::Center);
    b
}

/// Chip with a leading 7 px coloured dot (`bg-motion`, …).
pub fn chip_with_dot(text: &str, dot_class: &str) -> gtk::Button {
    let b = gtk::Button::new();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&dot(&["dot", dot_class]));
    row.append(&gtk::Label::new(Some(text)));
    b.set_child(Some(&row));
    b.add_css_class("chip");
    b.set_valign(gtk::Align::Center);
    b
}

/// Button with icon + label (e.g. header Home mode, Live).
pub fn icon_label_button(material: &str, px: i32, text: &str) -> gtk::Button {
    let b = gtk::Button::new();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&crate::icons::image(material, px));
    row.append(&gtk::Label::new(Some(text)));
    b.set_child(Some(&row));
    b.set_valign(gtk::Align::Center);
    b
}

/// Replace the label text inside an [`icon_label_button`].
pub fn set_button_text(button: &gtk::Button, text: &str) {
    if let Some(row) = button.child().and_then(|c| c.downcast::<gtk::Box>().ok())
        && let Some(label) = row
            .last_child()
            .and_then(|c| c.downcast::<gtk::Label>().ok())
    {
        label.set_text(text);
    }
}

/// Set or clear a CSS class.
pub fn set_class(w: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        w.add_css_class(class);
    } else {
        w.remove_css_class(class);
    }
}
