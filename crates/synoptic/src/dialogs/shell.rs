//! Dialog frame: header (icon, title, subtitle, ✕), body, optional footer.
//! Modal transient window with the `.dialog` CSS class; GTK has no page
//! scrim, so the dimmed backdrop from the prototype is approximated by the
//! modal window alone.

use gtk::prelude::*;

use crate::icons;
use crate::widgets::controls::{label, spacer};

pub struct Dialog {
    pub window: gtk::Window,
    pub body: gtk::Box,
    pub footer: gtk::Box,
}

/// Build and present a dialog of `width` px. `subtitle` renders mono/muted.
pub fn present(
    parent: &impl IsA<gtk::Window>,
    width: i32,
    icon: &str,
    title: &str,
    subtitle: Option<&str>,
) -> Dialog {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .decorated(false)
        .resizable(false)
        .default_width(width)
        .build();
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("dialog");

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    header.add_css_class("dialog-header");
    // a themed icon name (the app icon) is used as is; else a Material name
    if icon.contains('.') {
        let img = gtk::Image::from_icon_name(icon);
        img.set_pixel_size(24);
        header.append(&img);
    } else {
        header.append(&icons::image(icon, 18));
    }
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
    titles.set_valign(gtk::Align::Center);
    titles.append(&label(title, &["dialog-title"]));
    if let Some(s) = subtitle {
        titles.append(&label(s, &["dialog-subtitle"]));
    }
    header.append(&titles);
    header.append(&spacer());
    let close = icons::flat_button("close", "icon-32", "Close");
    {
        let window = window.clone();
        close.connect_clicked(move |_| window.close());
    }
    header.append(&close);
    frame.append(&header);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.add_css_class("dialog-body");
    frame.append(&body);

    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    footer.add_css_class("dialog-footer");
    footer.set_halign(gtk::Align::Fill);
    footer.set_visible(false);
    frame.append(&footer);

    window.set_child(Some(&frame));
    let esc = gtk::EventControllerKey::new();
    {
        let window = window.clone();
        esc.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                window.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    window.add_controller(esc);
    window.present();
    Dialog {
        window,
        body,
        footer,
    }
}

impl Dialog {
    /// Right-aligned footer buttons (Cancel first, primary last).
    pub fn set_footer(&self, buttons: &[&gtk::Button]) {
        self.footer.append(&spacer());
        for b in buttons {
            self.footer.append(*b);
        }
        self.footer.set_visible(true);
    }
}
