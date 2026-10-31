//! Modal dialogs. `install` listens for `Change::Request*` on the store and
//! opens the matching dialog on top of the main window, and kicks off the
//! silent auto-login at startup. `RequestLogout` is handled by `services`.

pub mod camera_settings;
pub mod login;
pub mod player;
pub mod snapshot_viewer;

pub mod shell;

use std::rc::Rc;

use gtk::glib;

use crate::services::Services;
use crate::state::{Change, Store};

pub fn install(window: &gtk::ApplicationWindow, store: &Rc<Store>, services: &Rc<Services>) {
    let window = window.clone();
    let store_w = Rc::downgrade(store);
    let services_rc = services.clone();
    store.subscribe(move |change| {
        let Some(store) = store_w.upgrade() else {
            return;
        };
        match change {
            Change::RequestLogin => login::open(&window, &store, &services_rc),
            Change::RequestCameraSettings(id) => camera_settings::open(&window, &store, *id),
            _ => {}
        }
    });

    // Auto-login once the window is up.
    let store = store.clone();
    let services = services.clone();
    glib::idle_add_local_once(move || login::auto_login(&store, &services));
}
