//! Modal dialogs. `install` listens for `Change::Request*` on the store and
//! opens the matching dialog on top of the main window.

pub mod camera_settings;
pub mod login;
pub mod player;
pub mod snapshot_viewer;

pub mod shell;

use std::rc::Rc;

use crate::services::Services;
use crate::state::{Change, Store};

pub fn install(window: &gtk::ApplicationWindow, store: &Rc<Store>, services: &Rc<Services>) {
    let window = window.clone();
    let store_w = Rc::downgrade(store);
    let services = services.clone();
    store.subscribe(move |change| {
        let Some(store) = store_w.upgrade() else {
            return;
        };
        match change {
            Change::RequestLogin => {
                let _ = (&window, &services);
                store.toast("Login dialog not implemented yet");
            }
            Change::RequestCameraSettings(id) => {
                store.toast(format!("Camera settings for {id} not implemented yet"));
            }
            _ => {}
        }
    });
}
