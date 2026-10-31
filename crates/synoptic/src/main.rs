//! Synoptic — GTK4 client for Synology Surveillance Station.
//!
//! Layout and visual values come from `design/handoff/README.md`; the CSS
//! tokens live in `data/style.css`; icons are Material Symbols Rounded SVGs
//! compiled into the binary as a GResource (`ms-<name>-symbolic`).
//! Module map and the parallel-work split: `docs/ui-plan.md`.

mod dialogs;
mod icons;
mod pages;
mod services;
mod state;
mod video;
mod widgets;

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use crate::services::Services;
use crate::state::{Change, Store, Theme};

pub const APP_ID: &str = "io.github.darious.Synoptic";
pub const RESOURCE_PREFIX: &str = "/io/github/darious/Synoptic";

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,synoptic=info,ss_api=info,ss_stream=info".into()),
        )
        .init();

    gio::resources_register_include!("synoptic.gresource").expect("embedded resources");
    gstreamer::init().expect("GStreamer init");

    let app = gtk::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| setup_styling());
    app.connect_activate(build_ui);
    app.run()
}

fn setup_styling() {
    let display = gdk::Display::default().expect("a display");
    // style.css holds the tokens and shell; the others belong to one page area
    // each so parallel work never touches the same stylesheet.
    for file in [
        "style.css",
        "style-live.css",
        "style-browse.css",
        "style-admin.css",
    ] {
        let provider = gtk::CssProvider::new();
        provider.load_from_resource(&format!("{RESOURCE_PREFIX}/{file}"));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    gtk::IconTheme::for_display(&display).add_resource_path(&format!("{RESOURCE_PREFIX}/icons"));
}

fn build_ui(app: &gtk::Application) {
    let store = Store::load();
    let services = Services::new();

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Synoptic")
        .default_width(1280)
        .default_height(800)
        .build();
    window.set_size_request(1100, 600);
    apply_theme(&window, store.get().theme);

    // header spans the full width; below it: rail | camera panel | stage
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&widgets::header::build(&store, &services));

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.set_vexpand(true);
    body.append(&widgets::rail::build(&store));
    body.append(&widgets::camera_panel::build(&store));
    let stage = pages::build(&store, &services);
    stage.set_hexpand(true);
    body.append(&stage);
    root.append(&body);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&root));
    widgets::toast::install(&overlay, &store);
    window.set_child(Some(&overlay));

    dialogs::install(&window, &store, &services);
    services::install(&store, &services);

    {
        let window = window.clone();
        let store_w = Rc::downgrade(&store);
        store.subscribe(move |change| {
            if let (Change::Theme, Some(store)) = (change, store_w.upgrade()) {
                apply_theme(&window, store.get().theme);
            }
        });
    }

    window.present();
}

fn apply_theme(window: &impl IsA<gtk::Widget>, theme: Theme) {
    let w = window.as_ref();
    match theme {
        Theme::Dark => {
            w.remove_css_class("light");
            w.add_css_class("dark");
        }
        Theme::Light => {
            w.remove_css_class("dark");
            w.add_css_class("light");
        }
    }
}
