//! settings page — stub until implemented (see docs/ui-plan.md).

use std::rc::Rc;

use crate::services::Services;
use crate::state::Store;

pub fn build(_store: &Rc<Store>, _services: &Rc<Services>) -> gtk::Widget {
    crate::pages::placeholder("settings — not implemented yet")
}
