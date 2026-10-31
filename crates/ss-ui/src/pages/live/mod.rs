//! live page(s) — see docs/ui-plan.md for the module plan. Stub until implemented.

pub mod grid;
pub mod slot;
pub mod timeline;

use std::rc::Rc;

use crate::services::Services;
use crate::state::Store;

pub fn build(_store: &Rc<Store>, _services: &Rc<Services>) -> gtk::Widget {
    crate::pages::placeholder("Live View — not implemented yet")
}
