//! Bottom-centre toast (36 h, 2.2 s), driven by `Change::Toast`.

use std::cell::Cell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use crate::icons;
use crate::state::{Change, Store};

pub fn install(overlay: &gtk::Overlay, store: &Rc<Store>) {
    let toast = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    toast.add_css_class("toast");
    toast.set_halign(gtk::Align::Center);
    toast.set_valign(gtk::Align::End);
    toast.set_visible(false);
    toast.append(&icons::image("check_circle", 16));
    let label = gtk::Label::new(None);
    toast.append(&label);
    overlay.add_overlay(&toast);

    let generation = Rc::new(Cell::new(0u64));
    store.subscribe(move |change| {
        let Change::Toast(text) = change else { return };
        label.set_text(text);
        toast.set_visible(true);
        let g = generation.get() + 1;
        generation.set(g);
        let toast = toast.clone();
        let generation = generation.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(2200), move || {
            if generation.get() == g {
                toast.set_visible(false);
            }
        });
    });
}
