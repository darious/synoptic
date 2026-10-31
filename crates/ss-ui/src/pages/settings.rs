//! Settings page: groups built from `ss_config::SETTINGS`, max-width 960.
//!
//! Each row reads its effective value through `AppConfig::setting`, writes
//! through `set_setting` / `reset_setting`, and shows the Reset button at
//! 30 % while the value is at its default.

use std::cell::Cell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use ss_config::{SETTINGS, SettingDef, SettingKind, SettingValue};

use crate::icons;
use crate::services::Services;
use crate::state::Store;
use crate::widgets::controls::{ellipsized, label, set_class, spacer};

/// Registry section → page group title, in the spec's display order.
const GROUPS: &[(&str, &str)] = &[
    ("Live View settings", "Live View"),
    ("Media player settings", "Media player"),
    ("Timeline settings", "Timeline"),
    ("POS receipt overlay", "POS receipt overlay"),
    ("Event settings", "Events"),
];

/// Group title for a registry section.
pub fn group_title(section: &str) -> &str {
    GROUPS
        .iter()
        .find(|(s, _)| *s == section)
        .map(|(_, t)| *t)
        .unwrap_or(section)
}

/// Sections in display order: the known ones first, then anything new.
pub fn ordered_sections() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = GROUPS
        .iter()
        .map(|(s, _)| *s)
        .filter(|s| SETTINGS.iter().any(|d| d.section == *s))
        .collect();
    for d in SETTINGS {
        if !out.contains(&d.section) {
            out.push(d.section);
        }
    }
    out
}

fn round_to(v: f64, digits: u8) -> f64 {
    let f = 10f64.powi(i32::from(digits));
    (v * f).round() / f
}

/// One spinner click: `dir` is −1 or +1; the result is clamped and rounded.
pub fn step_number(current: f64, dir: i32, min: f64, max: f64, step: f64, digits: u8) -> f64 {
    round_to((current + step * f64::from(dir)).clamp(min, max), digits)
}

pub fn format_number(v: f64, digits: u8) -> String {
    format!("{v:.*}", usize::from(digits))
}

// --- Clamp: centre a child and cap its width ----------------------------------

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Clamp {
        pub max_width: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Clamp {
        const NAME: &'static str = "SsClamp";
        type Type = super::Clamp;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Clamp {
        fn dispose(&self) {
            while let Some(c) = self.obj().first_child() {
                c.unparent();
            }
        }
    }

    impl WidgetImpl for Clamp {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.obj().first_child() else {
                return (0, 0, -1, -1);
            };
            match orientation {
                gtk::Orientation::Horizontal => {
                    let (min, nat, _, _) = child.measure(orientation, -1);
                    (min, nat.max(min), -1, -1)
                }
                _ => {
                    let w = if for_size < 0 {
                        -1
                    } else {
                        for_size.min(self.max_width.get())
                    };
                    let (min, nat, _, _) = child.measure(orientation, w);
                    (min, nat, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.obj().first_child() else {
                return;
            };
            let w = width.min(self.max_width.get());
            let x = (width - w) / 2;
            child.size_allocate(&gtk::Allocation::new(x, 0, w, height), baseline);
        }
    }
}

glib::wrapper! {
    /// A single-child container that centres its child and caps its width.
    pub struct Clamp(ObjectSubclass<imp::Clamp>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Clamp {
    pub fn new(max_width: i32, child: &impl IsA<gtk::Widget>) -> Self {
        let o: Self = glib::Object::new();
        o.imp().max_width.set(max_width);
        child.set_parent(&o);
        o.set_hexpand(true);
        o
    }
}

// --- page ----------------------------------------------------------------------

type Refresh = Rc<dyn Fn()>;

pub fn build(store: &Rc<Store>, _services: &Rc<Services>) -> gtk::Widget {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.add_css_class("settings");

    let mut refreshers: Vec<Refresh> = Vec::new();

    // top-right Reset all
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.append(&spacer());
    let reset_all = gtk::Button::new();
    let reset_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    reset_content.append(&icons::image("restart_alt", 16));
    reset_content.append(&gtk::Label::new(Some("Reset all to defaults")));
    reset_all.set_child(Some(&reset_content));
    top.append(&reset_all);
    column.append(&top);

    for section in ordered_sections() {
        let header = label(
            &group_title(section).to_uppercase(),
            &["section-header", "group-header", "group-title"],
        );
        column.append(&header);
        for def in SETTINGS.iter().filter(|d| d.section == section) {
            let (row, refresh) = setting_row(store, def);
            column.append(&row);
            refreshers.push(refresh);
        }
    }

    {
        let store = store.clone();
        let refreshers = refreshers.clone();
        reset_all.connect_clicked(move |_| {
            store.config().reset_all_settings();
            store.save_soon();
            for r in &refreshers {
                r();
            }
            store.toast("All settings reset to defaults");
        });
    }

    let clamp = Clamp::new(960, &column);
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_child(Some(&clamp));
    scroller.upcast()
}

/// Validate-and-store one value; the toast carries the registry's message.
fn set_value(store: &Rc<Store>, key: &str, value: SettingValue) {
    let r = store.config().set_setting(key, value);
    match r {
        Ok(()) => store.save_soon(),
        Err(e) => store.toast(e.to_string()),
    }
}

fn setting_row(store: &Rc<Store>, def: &'static SettingDef) -> (gtk::Box, Refresh) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("setting-row");
    row.set_tooltip_text(Some(def.tooltip));

    let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
    texts.set_hexpand(true);
    texts.set_valign(gtk::Align::Center);
    texts.set_margin_top(6);
    texts.set_margin_bottom(6);
    texts.append(&label(def.label, &["label"]));
    texts.append(&ellipsized(def.tooltip, &["hint"]));
    row.append(&texts);

    let reset = icons::flat_button("undo", "icon-30", "Reset to default");
    reset.add_css_class("reset");

    let key = def.key;
    let default = def.default_value();
    let current = {
        let store = store.clone();
        move || store.config().setting(key)
    };

    let refresh: Refresh = match def.kind {
        SettingKind::Number {
            min,
            max,
            step,
            digits,
            ..
        } => {
            let boxed = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            boxed.add_css_class("spinner-box");
            boxed.set_valign(gtk::Align::Center);
            let value = label("", &["value"]);
            value.set_xalign(1.0);
            boxed.append(&value);
            let minus = gtk::Button::new();
            minus.set_child(Some(&icons::image("remove", 16)));
            let plus = gtk::Button::new();
            plus.set_child(Some(&icons::image("add", 16)));
            boxed.append(&minus);
            boxed.append(&plus);
            row.append(&boxed);
            for (button, dir) in [(&minus, -1), (&plus, 1)] {
                let store = store.clone();
                let current = current.clone();
                button.connect_clicked(move |_| {
                    let now = match current() {
                        Some(SettingValue::Number(n)) => n,
                        _ => return,
                    };
                    let next = step_number(now, dir, min, max, step, digits);
                    if next != now {
                        set_value(&store, key, SettingValue::Number(next));
                    }
                });
            }
            Rc::new(move || {
                if let Some(SettingValue::Number(n)) = current() {
                    value.set_text(&format_number(n, digits));
                }
            })
        }
        SettingKind::Bool { .. } => {
            let switch = gtk::Switch::new();
            switch.set_valign(gtk::Align::Center);
            row.append(&switch);
            {
                let store = store.clone();
                let current = current.clone();
                switch.connect_active_notify(move |sw| {
                    if let Some(SettingValue::Bool(b)) = current()
                        && b != sw.is_active()
                    {
                        set_value(&store, key, SettingValue::Bool(sw.is_active()));
                    }
                });
            }
            Rc::new(move || {
                if let Some(SettingValue::Bool(b)) = current()
                    && switch.is_active() != b
                {
                    switch.set_active(b);
                }
            })
        }
        SettingKind::Choice { options, .. } => {
            let labels: Vec<&str> = options.iter().map(|(_, l)| *l).collect();
            let dropdown = gtk::DropDown::from_strings(&labels);
            dropdown.set_valign(gtk::Align::Center);
            row.append(&dropdown);
            {
                let store = store.clone();
                let current = current.clone();
                dropdown.connect_selected_notify(move |d| {
                    let Some((v, _)) = options.get(d.selected() as usize) else {
                        return;
                    };
                    if let Some(SettingValue::Choice(c)) = current()
                        && c != *v
                    {
                        set_value(&store, key, SettingValue::Choice((*v).to_string()));
                    }
                });
            }
            Rc::new(move || {
                if let Some(SettingValue::Choice(c)) = current()
                    && let Some(idx) = options.iter().position(|(v, _)| *v == c)
                    && dropdown.selected() != idx as u32
                {
                    dropdown.set_selected(idx as u32);
                }
            })
        }
    };

    // wrap: also update the Reset button state
    let refresh: Refresh = {
        let store = store.clone();
        let reset = reset.clone();
        Rc::new(move || {
            refresh();
            let at_default = store.config().setting(key).as_ref() == Some(&default);
            set_class(&reset, "at-default", at_default);
        })
    };
    {
        let store = store.clone();
        let refresh = refresh.clone();
        reset.connect_clicked(move |_| {
            store.config().reset_setting(key);
            store.save_soon();
            refresh();
        });
    }
    row.append(&reset);

    // keep rows in sync with external changes (Reset all, another row)
    refresh();
    (row, refresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_rounds_and_clamps() {
        assert_eq!(step_number(0.5, 1, 0.0, 10.0, 0.1, 2), 0.6);
        assert_eq!(step_number(0.0, -1, 0.0, 10.0, 0.1, 2), 0.0);
        assert_eq!(step_number(10.0, 1, 0.0, 10.0, 0.1, 2), 10.0);
        assert_eq!(step_number(32.0, 1, 1.0, 512.0, 1.0, 0), 33.0);
        assert_eq!(step_number(0.3, 1, 0.0, 1.0, 0.1, 2), 0.4);
    }

    #[test]
    fn number_formatting() {
        assert_eq!(format_number(0.5, 2), "0.50");
        assert_eq!(format_number(32.0, 0), "32");
    }

    #[test]
    fn every_registry_section_has_a_group() {
        let sections = ordered_sections();
        for d in SETTINGS {
            assert!(sections.contains(&d.section), "{}", d.section);
        }
        assert_eq!(group_title("Event settings"), "Events");
        assert_eq!(group_title("Brand new"), "Brand new");
    }
}
