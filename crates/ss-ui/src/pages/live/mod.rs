//! Live View page: the n×n grid over a 112 px timeline strip
//! (`design/handoff/README.md` §1). The grid owns the slots and their
//! streams; the timeline drives the grid's playback controller.

pub mod grid;
pub mod slot;
pub mod timeline;

use std::rc::Rc;

use gtk::prelude::*;

use crate::services::Services;
use crate::state::{Change, Store};

pub fn build(store: &Rc<Store>, services: &Rc<Services>) -> gtk::Widget {
    let stage = gtk::Box::new(gtk::Orientation::Vertical, 0);
    stage.add_css_class("live-stage");
    stage.set_hexpand(true);
    stage.set_vexpand(true);

    let grid = grid::Grid::new(store, services);
    stage.append(grid.widget());

    let timeline = timeline::build(store, services, &grid);
    timeline.set_visible(store.get().timeline_visible);
    stage.append(&timeline);

    {
        let store_w = Rc::downgrade(store);
        let timeline = timeline.clone();
        // The store outlives every page; this subscription also keeps the
        // grid controller (and with it the slots' streams) alive.
        let grid = grid.clone();
        store.subscribe(move |change| {
            let _keep = &grid;
            if let (Change::TimelineVisible, Some(store)) = (change, store_w.upgrade()) {
                timeline.set_visible(store.get().timeline_visible);
            }
        });
    }

    stage.upcast()
}
