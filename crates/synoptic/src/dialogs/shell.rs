//! Dialog frame: header (icon, title, subtitle, ✕), body, optional footer.
//!
//! Dialogs are layers inside the main window's `gtk::Overlay`, not separate
//! windows: a dimmed scrim over the whole window with the frame centred on
//! it. That gives the prototype's backdrop, never depends on the platform
//! placing a transient window (Win32 puts them at the screen corner) and
//! can never extend past the window, which itself fits the monitor. A body
//! taller than the window scrolls; header and footer stay visible.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

use crate::icons;
use crate::widgets::controls::{label, spacer};

type Callbacks = Rc<RefCell<Vec<Box<dyn Fn()>>>>;

/// Handle to an open dialog: close it, or run something when it closes.
/// Clone freely; closing twice is harmless.
#[derive(Clone)]
pub struct DialogHandle {
    overlay: gtk::Overlay,
    layer: gtk::Widget,
    frame: gtk::Box,
    closed: Callbacks,
    done: Rc<Cell<bool>>,
}

impl DialogHandle {
    pub fn close(&self) {
        if self.done.replace(true) {
            return;
        }
        for f in self.closed.borrow().iter() {
            f();
        }
        self.overlay.remove_overlay(&self.layer);
    }

    /// Called once, when the dialog closes by any route.
    pub fn connect_closed(&self, f: impl Fn() + 'static) {
        self.closed.borrow_mut().push(Box::new(f));
    }

    /// Controllers see events while focus is inside the dialog.
    pub fn add_controller(&self, controller: impl IsA<gtk::EventController>) {
        self.frame.add_controller(controller);
    }
}

pub struct Dialog {
    /// Named `window` for the call sites' `window.close()` habit.
    pub window: DialogHandle,
    pub body: gtk::Box,
    pub footer: gtk::Box,
}

/// Build and show a dialog of `width` px (less when the window is narrower)
/// over `parent`. `subtitle` renders mono/muted.
pub fn present(
    parent: &impl IsA<gtk::Window>,
    width: i32,
    icon: &str,
    title: &str,
    subtitle: Option<&str>,
) -> Dialog {
    let parent = parent.upcast_ref::<gtk::Window>();
    let overlay = parent
        .child()
        .and_downcast::<gtk::Overlay>()
        .expect("the main window's child is the shell overlay");

    let layer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    layer.add_css_class("dialog-scrim");
    layer.set_hexpand(true);
    layer.set_vexpand(true);
    // swallow clicks that reach the backdrop (modal); the frame's own
    // widgets claim theirs first, so only the scrim is affected
    let block = gtk::GestureClick::new();
    block.connect_pressed(|g, _, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
    });
    layer.add_controller(block);

    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("dialog");
    frame.set_halign(gtk::Align::Center);
    frame.set_valign(gtk::Align::Center);
    frame.set_focusable(true);
    let avail_w = parent.width();
    frame.set_width_request(if avail_w > 0 {
        width.min(avail_w - 32)
    } else {
        width
    });
    layer.append(&frame);

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
    header.append(&close);
    frame.append(&header);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.add_css_class("dialog-body");
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_propagate_natural_height(true);
    scroller.set_propagate_natural_width(true);
    scroller.set_child(Some(&body));
    let avail_h = parent.height();
    if avail_h > 0 {
        // header, footer and margins take about 140 px
        scroller.set_max_content_height((avail_h - 140).max(160));
    }
    frame.append(&scroller);

    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    footer.add_css_class("dialog-footer");
    footer.set_halign(gtk::Align::Fill);
    footer.set_visible(false);
    frame.append(&footer);

    let handle = DialogHandle {
        overlay: overlay.clone(),
        layer: layer.clone().upcast(),
        frame: frame.clone(),
        closed: Callbacks::default(),
        done: Rc::new(Cell::new(false)),
    };
    {
        let h = handle.clone();
        close.connect_clicked(move |_| h.close());
    }
    let esc = gtk::EventControllerKey::new();
    {
        let h = handle.clone();
        esc.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                h.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    frame.add_controller(esc);

    overlay.add_overlay(&layer);
    frame.grab_focus();
    Dialog {
        window: handle,
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
