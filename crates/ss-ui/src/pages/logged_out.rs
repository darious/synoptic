//! Stage content while logged out: icon, message, Login button.

use std::rc::Rc;

use gtk::prelude::*;

use crate::icons;
use crate::state::{Change, Store};

pub fn build(store: &Rc<Store>) -> gtk::Widget {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 12);
    b.add_css_class("empty-stage");
    b.set_valign(gtk::Align::Center);
    b.set_halign(gtk::Align::Center);
    b.append(&icons::image("videocam_off", 40));
    let l = gtk::Label::new(Some("Not connected to a NAS"));
    l.add_css_class("t14");
    l.add_css_class("w600");
    b.append(&l);
    let login = gtk::Button::with_label("Login");
    login.add_css_class("primary");
    login.set_halign(gtk::Align::Center);
    let store = store.clone();
    login.connect_clicked(move |_| store.emit(&Change::RequestLogin));
    b.append(&login);
    b.upcast()
}
