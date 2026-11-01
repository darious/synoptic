//! Material Symbols Rounded icon names → GTK icon names.
//!
//! The SVGs live in `data/icons/ms-<name>-symbolic.svg` and are compiled into
//! the GResource under `icons/scalable/actions/`, so GTK treats them as
//! symbolic icons and recolours them with the CSS `color`.
//! `name` is the Material name used in the UI spec
//! (`videocam`, `left_panel_close`, …). Filled variants: `play_arrow_fill`.

use gtk::prelude::*;

/// GTK icon name for a Material Symbols name.
pub fn name(material: &str) -> String {
    format!("ms-{material}-symbolic")
}

/// A `gtk::Image` showing the icon at `px` pixels.
pub fn image(material: &str, px: i32) -> gtk::Image {
    let img = gtk::Image::from_icon_name(&name(material));
    img.set_pixel_size(px);
    img
}

/// A borderless icon-only button (`.flat`) of the given square size class
/// (`icon-32`, `icon-30`, …).
pub fn flat_button(material: &str, size_class: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::new();
    b.set_child(Some(&image(material, icon_px(size_class))));
    b.add_css_class("flat");
    b.add_css_class(size_class);
    b.set_tooltip_text(Some(tooltip));
    b.set_valign(gtk::Align::Center);
    b
}

/// A bordered icon-only button of the given size class.
pub fn bordered_button(material: &str, size_class: &str, tooltip: &str) -> gtk::Button {
    let b = flat_button(material, size_class, tooltip);
    b.remove_css_class("flat");
    b
}

pub fn icon_px(size_class: &str) -> i32 {
    match size_class {
        "icon-32" => 18,
        "icon-34" | "icon-28" | "icon-24" => 16,
        _ => 17,
    }
}
