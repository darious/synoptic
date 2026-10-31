//! 56 px page rail with the 3 px active indicator and the About update dot.

use std::rc::Rc;

use gtk::prelude::*;

use crate::icons;
use crate::state::{Change, Page, Store};
use crate::widgets::controls::set_class;

pub fn build(store: &Rc<Store>) -> gtk::Box {
    let rail = gtk::Box::new(gtk::Orientation::Vertical, 4);
    rail.add_css_class("rail");
    rail.set_valign(gtk::Align::Fill);

    let mut entries: Vec<(Page, gtk::Button, gtk::Box, Option<gtk::Box>)> = Vec::new();

    for page in Page::ALL {
        let button = gtk::Button::new();
        button.set_tooltip_text(Some(page.title()));
        button.set_widget_name(&format!("rail-{}", page.key()));
        let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
        content.set_valign(gtk::Align::Center);
        content.set_halign(gtk::Align::Center);
        content.append(&icons::image(page.icon(), 22));
        let rail_label = gtk::Label::new(Some(page.title()));
        rail_label.add_css_class("rail-label");
        rail_label.set_visible(store.get().rail_labels);
        content.append(&rail_label);
        button.set_child(Some(&content));

        let indicator = gtk::Box::new(gtk::Orientation::Vertical, 0);
        indicator.add_css_class("indicator");
        indicator.set_halign(gtk::Align::Start);
        indicator.set_valign(gtk::Align::Fill);
        indicator.set_can_target(false);

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&button));
        overlay.add_overlay(&indicator);
        overlay.set_halign(gtk::Align::Center);

        let dot = if page == Page::About {
            let d = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            d.add_css_class("dot-red");
            d.set_halign(gtk::Align::End);
            d.set_valign(gtk::Align::Start);
            d.set_margin_top(8);
            d.set_margin_end(8);
            d.set_can_target(false);
            overlay.add_overlay(&d);
            Some(d)
        } else {
            None
        };

        {
            let store = store.clone();
            button.connect_clicked(move |_| {
                if store.get().page != page {
                    store.update(Change::Page, |s| {
                        s.page = page;
                        s.selected_slot = None;
                    });
                }
            });
        }
        rail.append(&overlay);
        entries.push((page, button, indicator, dot));
    }

    let refresh = {
        let store = store.clone();
        move || {
            let s = store.get();
            for (page, button, indicator, dot) in &entries {
                let active = s.page == *page;
                set_class(button, "active", active);
                indicator.set_visible(active);
                button.set_sensitive(s.logged_in() || page.available_offline());
                if let Some(d) = dot {
                    d.set_visible(s.update_notice_unread);
                }
                if let Some(content) = button.child().and_then(|c| c.downcast::<gtk::Box>().ok())
                    && let Some(label) = content.last_child()
                {
                    label.set_visible(s.rail_labels);
                }
            }
        }
    };
    refresh();
    store.subscribe(move |change| {
        if matches!(
            change,
            Change::Page | Change::Connection | Change::UpdateNotice
        ) {
            refresh();
        }
    });

    rail
}
