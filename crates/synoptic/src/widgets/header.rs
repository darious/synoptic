//! 48 px header bar.

use std::rc::Rc;

use gtk::prelude::*;

use crate::icons;
use crate::services::Services;
use crate::state::{Change, Page, Store, Theme};
use crate::widgets::controls::{
    divider_v, icon_label_button, label, set_button_text, set_class, spacer,
};
use crate::widgets::popovers;

pub fn build(store: &Rc<Store>, services: &Rc<Services>) -> gtk::Box {
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.add_css_class("header");

    // 1. logo
    let logo = gtk::Image::from_icon_name(crate::APP_ID);
    logo.set_pixel_size(28);
    logo.add_css_class("logo");
    logo.set_tooltip_text(Some("Synoptic"));
    logo.set_valign(gtk::Align::Center);
    header.append(&logo);

    // 2. panel toggle
    let panel_toggle = icons::flat_button("left_panel_close", "icon-32", "Hide camera panel");
    panel_toggle.set_widget_name("panel-toggle");
    {
        let store = store.clone();
        panel_toggle.connect_clicked(move |_| {
            store.update(Change::PanelVisible, |s| s.panel_visible = !s.panel_visible);
        });
    }
    header.append(&panel_toggle);

    // 3. Home Mode
    let home_mode = icon_label_button("home", 18, "Home mode");
    home_mode.add_css_class("home-mode");
    home_mode.set_widget_name("home-mode");
    {
        let store = store.clone();
        home_mode.connect_clicked(move |_| {
            let on = !store.get().home_mode;
            store.update(Change::HomeMode, |s| s.home_mode = on);
            store.toast(if on {
                "Home mode turned on"
            } else {
                "Home mode turned off"
            });
        });
    }
    header.append(&home_mode);

    // 4. divider, 5. title + connection
    let title_divider = divider_v();
    header.append(&title_divider);
    let title = label("Live View", &["page-title"]);
    header.append(&title);
    let conn = label("· not connected", &["conn-label"]);
    header.append(&conn);

    // 6. spacer
    header.append(&spacer());

    // 7. Live View controls
    let live_controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let layout_button = gtk::MenuButton::new();
    layout_button.add_css_class("icon-32");
    layout_button.set_widget_name("layout-menu");
    layout_button.set_valign(gtk::Align::Center);
    let layout_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    layout_content.append(&icons::image("grid_view", 18));
    let layout_label = gtk::Label::new(Some("2x2"));
    layout_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    layout_label.set_max_width_chars(22);
    layout_content.append(&layout_label);
    layout_content.append(&icons::image("expand_more", 16));
    layout_button.set_child(Some(&layout_content));
    layout_button.set_popover(Some(&popovers::layout_menu(store)));
    live_controls.append(&layout_button);

    let reload = icons::bordered_button("refresh", "icon-32", "Reload streams");
    reload.set_widget_name("reload");
    {
        let store = store.clone();
        reload.connect_clicked(move |_| {
            store.emit(&Change::RequestReload);
            store.toast("Reloading every stream in the layout");
        });
    }
    live_controls.append(&reload);

    let timeline = icons::bordered_button("view_timeline", "icon-32", "Toggle timeline");
    timeline.set_widget_name("timeline-toggle");
    {
        let store = store.clone();
        timeline.connect_clicked(move |_| {
            store.update(Change::TimelineVisible, |s| {
                s.timeline_visible = !s.timeline_visible;
            });
        });
    }
    live_controls.append(&timeline);
    live_controls.append(&divider_v());
    header.append(&live_controls);

    // 8. bell with badge
    let bell_button = gtk::MenuButton::new();
    bell_button.add_css_class("flat");
    bell_button.set_widget_name("bell");
    bell_button.add_css_class("icon-32");
    bell_button.set_valign(gtk::Align::Center);
    let bell_overlay = gtk::Overlay::new();
    bell_overlay.set_child(Some(&icons::image("notifications", 20)));
    let badge = gtk::Label::new(Some("0"));
    badge.add_css_class("badge");
    badge.set_halign(gtk::Align::End);
    badge.set_valign(gtk::Align::Start);
    badge.set_can_target(false);
    bell_overlay.add_overlay(&badge);
    bell_button.set_child(Some(&bell_overlay));
    bell_button.set_popover(Some(&popovers::notifications(store, services)));
    header.append(&bell_button);

    // 9. theme toggle
    let theme_button = icons::flat_button("light_mode", "icon-32", "Switch theme");
    theme_button.set_widget_name("theme");
    {
        let store = store.clone();
        theme_button.connect_clicked(move |_| {
            store.update(Change::Theme, |s| {
                s.theme = match s.theme {
                    Theme::Dark => Theme::Light,
                    Theme::Light => Theme::Dark,
                }
            });
        });
    }
    header.append(&theme_button);

    // 10. avatar / Login
    let avatar = gtk::MenuButton::new();
    avatar.add_css_class("avatar");
    avatar.set_widget_name("avatar");
    avatar.set_valign(gtk::Align::Center);
    let initials = gtk::Label::new(Some("??"));
    avatar.set_child(Some(&initials));
    avatar.set_popover(Some(&popovers::account(store)));
    header.append(&avatar);

    let login = gtk::Button::with_label("Login");
    login.add_css_class("primary");
    login.set_widget_name("login");
    login.set_valign(gtk::Align::Center);
    login.set_size_request(-1, 32);
    {
        let store = store.clone();
        login.connect_clicked(move |_| store.emit(&Change::RequestLogin));
    }
    header.append(&login);

    let refresh = {
        let store = store.clone();
        move || {
            let s = store.get();
            let logged_in = s.logged_in();
            title.set_text(s.page.title());
            conn.set_text(&match &s.connection {
                Some(c) => format!("· {} · {}", c.profile, c.host),
                None => "· not connected".to_string(),
            });
            if let Some(img) = panel_toggle
                .child()
                .and_then(|c| c.downcast::<gtk::Image>().ok())
            {
                img.set_icon_name(Some(&icons::name(if s.panel_visible {
                    "left_panel_close"
                } else {
                    "left_panel_open"
                })));
            }
            home_mode.set_visible(logged_in);
            set_class(&home_mode, "on", s.home_mode);
            set_button_text(
                &home_mode,
                if s.home_mode {
                    "Home mode on"
                } else {
                    "Home mode"
                },
            );
            live_controls.set_visible(logged_in && s.page == Page::Live);
            layout_label.set_text(&match s.layout() {
                Some(l) => format!("{} · {}", l.name, l.shape.key()),
                None => s.shape().key().to_string(),
            });
            set_class(&timeline, "on", s.timeline_visible);
            bell_button.set_visible(logged_in);
            badge.set_visible(s.unread_notifications > 0);
            badge.set_text(&s.unread_notifications.to_string());
            if let Some(img) = theme_button
                .child()
                .and_then(|c| c.downcast::<gtk::Image>().ok())
            {
                img.set_icon_name(Some(&icons::name(match s.theme {
                    Theme::Dark => "light_mode",
                    Theme::Light => "dark_mode",
                })));
            }
            avatar.set_visible(logged_in);
            login.set_visible(!logged_in);
            if let Some(c) = &s.connection {
                initials.set_text(&initials_of(&c.username));
            }
        }
    };
    refresh();
    store.subscribe(move |change| {
        if matches!(
            change,
            Change::Page
                | Change::Connection
                | Change::PanelVisible
                | Change::HomeMode
                | Change::Layout
                | Change::Slots
                | Change::TimelineVisible
                | Change::Notifications
                | Change::Theme
        ) {
            refresh();
        }
    });

    header
}

fn initials_of(name: &str) -> String {
    let mut s: String = name
        .split(|c: char| !c.is_alphanumeric())
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect();
    if s.chars().count() < 2 {
        s = name.chars().take(2).collect();
    }
    s.to_uppercase()
}
