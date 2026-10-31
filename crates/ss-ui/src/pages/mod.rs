//! The stage: one `gtk::Stack` child per page plus the logged-out state.
//!
//! Each page module exposes `build(store, services) -> gtk::Widget`.
//! Ownership during parallel work: see `docs/ui-plan.md`.

pub mod about;
pub mod browse;
pub mod licenses;
pub mod live;
pub mod logged_out;
pub mod settings;

use std::rc::Rc;

use gtk::prelude::*;

use crate::services::Services;
use crate::state::{Change, Page, Store};

pub const LOGGED_OUT: &str = "logged-out";

pub fn build(store: &Rc<Store>, services: &Rc<Services>) -> gtk::Stack {
    let stack = gtk::Stack::new();
    stack.add_css_class("stage");
    stack.set_transition_type(gtk::StackTransitionType::None);

    stack.add_named(&logged_out::build(store), Some(LOGGED_OUT));
    stack.add_named(&live::build(store, services), Some(Page::Live.key()));
    for page in [
        Page::Recordings,
        Page::Snapshots,
        Page::Events,
        Page::TimeLapse,
    ] {
        stack.add_named(&browse::build(store, services, page), Some(page.key()));
    }
    stack.add_named(
        &licenses::build(store, services),
        Some(Page::Licenses.key()),
    );
    stack.add_named(
        &settings::build(store, services),
        Some(Page::Settings.key()),
    );
    stack.add_named(&about::build(store, services), Some(Page::About.key()));

    let select = {
        let store = store.clone();
        let stack = stack.clone();
        move || {
            let s = store.get();
            let name = if s.logged_in() || s.page.available_offline() {
                s.page.key()
            } else {
                LOGGED_OUT
            };
            stack.set_visible_child_name(name);
        }
    };
    select();
    store.subscribe(move |c| {
        if matches!(c, Change::Page | Change::Connection) {
            select();
        }
    });
    stack
}

/// Placeholder used by pages that are not implemented yet.
pub fn placeholder(text: &str) -> gtk::Widget {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 8);
    b.set_valign(gtk::Align::Center);
    b.set_halign(gtk::Align::Center);
    let l = gtk::Label::new(Some(text));
    l.add_css_class("muted");
    b.append(&l);
    b.upcast()
}
