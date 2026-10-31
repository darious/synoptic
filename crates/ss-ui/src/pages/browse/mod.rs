//! browse page(s) — see docs/ui-plan.md for the module plan. Stub until implemented.

pub mod events;
pub mod filter_bar;
pub mod pagination;
pub mod recordings;
pub mod snapshots;
pub mod timelapse;

use std::rc::Rc;

use crate::services::Services;
use crate::state::Page;
use crate::state::Store;

pub fn build(_store: &Rc<Store>, _services: &Rc<Services>, page: Page) -> gtk::Widget {
    crate::pages::placeholder(&format!("{} — not implemented yet", page.title()))
}
