//! Shared filter bar of the browse pages.
//!
//! Row 1: QUICK FILTER label, preset pills, (Events) type pills, camera
//! filter box, Refresh, Advanced toggle, Reset. Advanced row: From / To,
//! camera chip buttons, Search. Summary row: `Time · Camera · Type`.

use std::rc::Rc;

use gtk::prelude::*;

use super::{Browse, EventType, Preset};
use crate::icons;
use crate::state::Page;
use crate::widgets::controls::{chip, chip_with_dot, divider_v, label, set_class, spacer};

pub fn build(ctx: &Rc<Browse>) -> gtk::Widget {
    let bar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bar.add_css_class("filter-bar");

    // ----- row 1 ---------------------------------------------------------
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("filter-row");
    row.append(&label("QUICK FILTER", &["filter-label"]));

    let mut preset_chips = Vec::new();
    for p in Preset::ALL {
        let c = chip(p.label());
        {
            let ctx = ctx.clone();
            c.connect_clicked(move |_| {
                ctx.filter.borrow_mut().preset = Some(p);
                ctx.filter_changed();
            });
        }
        row.append(&c);
        preset_chips.push((p, c));
    }

    let mut type_chips = Vec::new();
    if ctx.page == Page::Events {
        row.append(&divider_v());
        for t in EventType::ALL {
            let c = chip_with_dot(t.label(), &format!("bg-{}", t.css()));
            {
                let ctx = ctx.clone();
                c.connect_clicked(move |_| {
                    {
                        let mut f = ctx.filter.borrow_mut();
                        f.event_type = if f.event_type == Some(t) {
                            None
                        } else {
                            Some(t)
                        };
                    }
                    ctx.filter_changed();
                });
            }
            row.append(&c);
            type_chips.push((t, c));
        }
    }

    row.append(&spacer());

    // camera filter display
    let cam_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    cam_box.add_css_class("cam-filter");
    cam_box.set_valign(gtk::Align::Center);
    let cam_icon = if ctx.page == Page::TimeLapse {
        "timelapse"
    } else {
        "videocam"
    };
    let cam_img = icons::image(cam_icon, 16);
    cam_img.add_css_class("muted");
    cam_box.append(&cam_img);
    let cam_label = label("All cameras", &["t12", "w500"]);
    cam_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    cam_label.set_max_width_chars(28);
    cam_box.append(&cam_label);
    let cam_clear = icons::flat_button("close", "icon-24", "Clear camera filter");
    cam_clear.add_css_class("cam-clear");
    cam_clear.set_visible(false);
    {
        let ctx = ctx.clone();
        cam_clear.connect_clicked(move |_| ctx.set_camera(None));
    }
    cam_box.append(&cam_clear);
    row.append(&cam_box);

    let refresh = icons::bordered_button("refresh", "icon-30", "Refresh");
    {
        let ctx = ctx.clone();
        refresh.connect_clicked(move |_| ctx.refresh());
    }
    row.append(&refresh);

    let advanced_toggle = icons::bordered_button("manage_search", "icon-30", "Advanced search");
    {
        let ctx = ctx.clone();
        advanced_toggle.connect_clicked(move |_| {
            {
                let mut f = ctx.filter.borrow_mut();
                f.advanced_open = !f.advanced_open;
            }
            ctx.notify_filter();
        });
    }
    row.append(&advanced_toggle);

    let reset = icons::bordered_button("backspace", "icon-30", "Reset filters");
    {
        let ctx = ctx.clone();
        reset.connect_clicked(move |_| {
            ctx.filter.borrow_mut().reset();
            // Clearing the camera goes through the store; it reloads via
            // the FilterCamera subscription when something changed.
            let had_camera = ctx.filter.borrow().camera.is_some();
            ctx.set_camera(None);
            if !had_camera {
                ctx.filter_changed();
            }
        });
    }
    row.append(&reset);
    bar.append(&row);

    // ----- advanced row ------------------------------------------------------
    let adv = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    adv.add_css_class("advanced-row");
    adv.set_visible(false);
    let from = gtk::Entry::new();
    from.add_css_class("mono");
    from.set_placeholder_text(Some("2026-10-11 00:00"));
    from.set_tooltip_text(Some("From (YYYY-MM-DD HH:MM)"));
    from.set_width_chars(16);
    let to = gtk::Entry::new();
    to.add_css_class("mono");
    to.set_placeholder_text(Some("2026-10-11 23:59"));
    to.set_tooltip_text(Some("To (YYYY-MM-DD HH:MM)"));
    to.set_width_chars(16);
    adv.append(&label("From", &["t12", "muted"]));
    adv.append(&from);
    adv.append(&label("To", &["t12", "muted"]));
    adv.append(&to);

    let cam_chips = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    cam_chips.add_css_class("cam-chips");
    adv.append(&cam_chips);
    adv.append(&spacer());

    let search = gtk::Button::new();
    {
        let inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        inner.append(&icons::image("search", 16));
        inner.append(&gtk::Label::new(Some("Search")));
        search.set_child(Some(&inner));
    }
    search.add_css_class("primary");
    search.set_valign(gtk::Align::Center);
    adv.append(&search);
    bar.append(&adv);

    let apply_advanced = {
        let ctx = ctx.clone();
        let (from, to) = (from.clone(), to.clone());
        Rc::new(move || {
            let f_text = from.text().trim().to_string();
            let t_text = to.text().trim().to_string();
            {
                let mut f = ctx.filter.borrow_mut();
                f.from = f_text.clone();
                f.to = t_text.clone();
                f.preset = if f_text.is_empty() && t_text.is_empty() {
                    Some(Preset::Today)
                } else {
                    None
                };
            }
            if (!f_text.is_empty() && super::parse_local(&f_text).is_none())
                || (!t_text.is_empty() && super::parse_local(&t_text).is_none())
            {
                ctx.toast("Use YYYY-MM-DD HH:MM for From / To");
            }
            ctx.filter_changed();
        })
    };
    {
        let apply = apply_advanced.clone();
        search.connect_clicked(move |_| apply());
    }
    {
        let apply = apply_advanced.clone();
        from.connect_activate(move |_| apply());
    }
    {
        let apply = apply_advanced.clone();
        to.connect_activate(move |_| apply());
    }

    // ----- summary row -------------------------------------------------------
    let summary = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    summary.add_css_class("summary-row");
    let s_icon = icons::image("filter_list", 14);
    s_icon.add_css_class("muted");
    summary.append(&s_icon);
    let summary_label = label("", &["t11", "muted"]);
    summary_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    summary.append(&summary_label);
    bar.append(&summary);

    // ----- sync from the filter state ---------------------------------------
    let sync = {
        let ctx_w = Rc::downgrade(ctx);
        let adv = adv.clone();
        let (from, to) = (from.clone(), to.clone());
        let cam_chips = cam_chips.clone();
        let advanced_toggle = advanced_toggle.clone();
        let cam_label = cam_label.clone();
        let cam_clear = cam_clear.clone();
        let summary_label = summary_label.clone();
        move || {
            let Some(ctx) = ctx_w.upgrade() else { return };
            let f = ctx.filter.borrow().clone();
            for (p, c) in &preset_chips {
                set_class(c, "active", f.preset == Some(*p));
            }
            for (t, c) in &type_chips {
                set_class(c, "active", f.event_type == Some(*t));
            }
            let cam_name = f.camera.and_then(|id| ctx.camera_name(id));
            cam_label.set_text(cam_name.as_deref().unwrap_or("All cameras"));
            cam_clear.set_visible(f.camera.is_some());
            set_class(&advanced_toggle, "on", f.advanced_open);
            adv.set_visible(f.advanced_open);
            if from.text().as_str() != f.from && !from.has_focus() {
                from.set_text(&f.from);
            }
            if to.text().as_str() != f.to && !to.has_focus() {
                to.set_text(&f.to);
            }
            // camera chip buttons: rebuilt from the camera list
            super::clear_box(&cam_chips);
            let cameras: Vec<(i64, String)> = ctx
                .store
                .get()
                .cameras
                .iter()
                .map(|c| (c.id, c.name.clone()))
                .collect();
            for (id, name) in cameras {
                let b = gtk::Button::with_label(&name);
                b.add_css_class("cam-chip");
                b.set_valign(gtk::Align::Center);
                set_class(&b, "selected", f.camera == Some(id));
                let ctx = ctx.clone();
                b.connect_clicked(move |_| {
                    let current = ctx.filter.borrow().camera;
                    ctx.set_camera(if current == Some(id) { None } else { Some(id) });
                });
                cam_chips.append(&b);
            }
            summary_label.set_text(&ctx.summary());
        }
    };
    sync();
    ctx.on_filter(sync);

    bar.upcast()
}
