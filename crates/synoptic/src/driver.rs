//! Headless test driver. Enabled by `SYNOPTIC_DRIVER=<fifo or file path>`:
//! lines read from that path are executed on the GTK main loop, and results
//! are printed to stdout as `ok …` / `err …`. Together with GTK's Broadway
//! backend this lets the app be exercised and screenshotted on a machine
//! without a display:
//!
//! ```text
//! gtk4-broadwayd :7 &
//! mkfifo /tmp/synoptic.cmd
//! GDK_BACKEND=broadway BROADWAY_DISPLAY=:7 SYNOPTIC_DRIVER=/tmp/synoptic.cmd synoptic &
//! echo 'shot /tmp/out.png' > /tmp/synoptic.cmd
//! ```
//!
//! Commands (one per line):
//!
//! | command | effect |
//! |---|---|
//! | `shot <path.png>` | render every visible toplevel; dialogs get `-2`, `-3`… suffixes, open popovers `-popover`, `-popover2`… |
//! | `login <profile> <host> <port> <user> <password> [otp]` | plain-HTTP login, like the dialog |
//! | `logout` | sign out |
//! | `page <live\|recordings\|snapshots\|events\|timelapse\|licenses\|settings\|about>` | switch page |
//! | `layout <shape>` | reshape the active layout, e.g. `2x3`, `1+5` |
//! | `layouts` | list the saved layouts (`*` marks the active one) |
//! | `layout-new <shape> [name…]` | add a layout (auto-filled in panel order) and switch to it |
//! | `layout-use <index\|name>` | switch layout |
//! | `layout-rename <name…>`, `layout-duplicate`, `layout-delete`, `layout-clear` | layout actions |
//! | `slot <index> <camera_id\|0>` | put a camera in a slot of the current layout |
//! | `drag <camera_id> <slot>` | what dropping a panel row on a slot does |
//! | `swap <slot> <slot>` | what dropping a slot on another slot does |
//! | `reorder <camera_id> <before_camera_id\|end>` | what dropping a panel row on another row does |
//! | `select <index\|none>` | select a slot |
//! | `focus <index\|none>` | what double-clicking a slot does: show it alone (one timeline lane), or back to the layout |
//! | `theme <dark\|light>`, `panel <on\|off>`, `timeline <on\|off>` | shell toggles |
//! | `click <widget-name>` | activate a named widget (button, menu button, toggle, row) |
//! | `type <widget-name> <text…>` | set an entry's text |
//! | `show <widget-name>`, `hide <widget-name>` | force a named widget visible/hidden (e.g. a slot's hover toolbar) |
//! | `player [camera_id]` | open the Player dialog on the newest recording (of that camera) |
//! | `ptzwin <camera_id>` | show the PTZ pad for a camera in a plain window (Broadway cannot map popovers) |
//! | `release <version> [url]` | pretend the update check found this release (About notice, rail dot) |
//! | `names` | list named widgets with type and visibility |
//! | `sleep <ms>` | wait before the next command |
//! | `shotw <widget-name> <path.png>` | render one named widget (e.g. a popover's child) |
//! | `quit` | exit the app |
//!
//! Timeline (Live View; see `pages/live/timeline.rs::driver_command`):
//!
//! | command | effect |
//! |---|---|
//! | `seek <unix-secs\|-<secs>>` | seek every slot (`-600` = ten minutes ago); the live edge goes live |
//! | `golive` | back to live, window follows now again |
//! | `step <±secs>` | move the position (`-10`, `60`) |
//! | `zoom <in\|out\|secs>` | zoom level, e.g. `zoom 86400` |
//! | `pan <±secs>` | shift the window (stops following live) |
//! | `tl-filter <motion\|alarm\|manual\|other\|recordings> <on\|off>` | event filter toggles |
//! | `tl-hover <frac 0..1> <lane\|ruler\|off>` | fake a pointer position (tooltip bubble) |
//! | `tl-date <YYYY-MM-DD> <HH:MM>` | set the date picker and go |
//! | `tl-state` | print mode, position, window, zoom, lanes, cached events |

use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use crate::services::{self, ConnectParams, Services};
use crate::state::{Change, Page, Shape, Store, Theme};

pub const ENV: &str = "SYNOPTIC_DRIVER";

pub fn install(app: &gtk::Application, store: &Rc<Store>, services: &Rc<Services>) {
    let Ok(path) = std::env::var(ENV) else {
        return;
    };
    let (tx, rx) = async_channel::unbounded::<String>();
    std::thread::Builder::new()
        .name("synoptic-driver".into())
        .spawn(move || {
            use std::io::BufRead;
            // read+write keeps a FIFO open across writers (no EOF when one closes)
            let file = match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .or_else(|_| std::fs::File::open(&path))
            {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("driver: cannot open {path}: {e}");
                    return;
                }
            };
            for line in std::io::BufReader::new(file).lines() {
                match line {
                    Ok(l) => {
                        if tx.send_blocking(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .expect("driver thread");

    let app = app.clone();
    let store = store.clone();
    let services = services.clone();
    glib::spawn_future_local(async move {
        println!("driver ready");
        while let Ok(line) = rx.recv().await {
            let line = line.trim().to_string();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match run(&app, &store, &services, &line).await {
                Ok(msg) => println!(
                    "ok {line}{}",
                    if msg.is_empty() {
                        String::new()
                    } else {
                        format!(" — {msg}")
                    }
                ),
                Err(e) => println!("err {line} — {e}"),
            }
        }
    });
}

async fn run(
    app: &gtk::Application,
    store: &Rc<Store>,
    services: &Rc<Services>,
    line: &str,
) -> Result<String, String> {
    let mut parts = line.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let args: Vec<&str> = parts.collect();
    match cmd {
        "shot" => {
            let path = args.first().ok_or("usage: shot <path.png>")?;
            // A widget only has a render node once a frame has been drawn
            // after the last invalidation; on Broadway without a connected
            // browser frames are sparse, so poke and retry.
            let mut last = Err(String::new());
            for _ in 0..12 {
                for w in toplevels() {
                    w.queue_draw();
                }
                glib::timeout_future(std::time::Duration::from_millis(200)).await;
                last = shot(path);
                if last.is_ok() {
                    break;
                }
            }
            last
        }
        "login" => {
            if args.len() < 5 {
                return Err("usage: login <profile> <host> <port> <user> <password> [otp]".into());
            }
            let params = ConnectParams {
                profile: args[0].to_string(),
                host: args[1].to_string(),
                port: args[2].parse().map_err(|_| "bad port")?,
                https: false,
                verify_ssl: false,
                username: args[3].to_string(),
                password: args[4].to_string(),
                otp_code: args.get(5).map(|s| s.to_string()),
                trust_device: false,
                device_id: None,
            };
            match services.connect(params.clone()).await {
                Ok(c) => {
                    let n = c.cameras.len();
                    // like the login dialog: the profile must exist for its
                    // layouts / camera order to be persisted
                    {
                        let mut cfg = store.config();
                        if !cfg.profiles.contains_key(&params.profile) {
                            let profile = ss_config::Profile {
                                port: params.port,
                                https: params.https,
                                verify_ssl: params.verify_ssl,
                                ..ss_config::Profile::new(params.host.clone())
                            };
                            cfg.add_profile(params.profile.clone(), profile);
                        }
                    }
                    services::apply_connected(store, &params, c);
                    Ok(format!("{n} cameras"))
                }
                Err(e) => Err(format!("{e:?}")),
            }
        }
        "logout" => {
            store.emit(&Change::RequestLogout);
            Ok(String::new())
        }
        "page" => {
            let key = args.first().ok_or("usage: page <key>")?;
            let page = Page::ALL
                .into_iter()
                .find(|p| p.key() == *key)
                .ok_or("unknown page")?;
            store.update(Change::Page, |s| {
                s.page = page;
                s.selected_slot = None;
            });
            Ok(String::new())
        }
        "layout" => {
            let key = args.first().ok_or("usage: layout <shape>")?;
            let shape = Shape::from_key(key).ok_or("unknown shape")?;
            store.update(Change::Layout, |s| s.set_shape(shape));
            store.emit(&Change::Slots);
            Ok(String::new())
        }
        "layouts" => {
            let s = store.get();
            let mut out = Vec::new();
            for (i, l) in s.layouts.iter().enumerate() {
                out.push(format!(
                    "  {}{i} {} {} {:?}",
                    if i == s.active_layout { "*" } else { " " },
                    l.name,
                    l.shape.key(),
                    l.cameras
                ));
            }
            println!("{}", out.join("\n"));
            Ok(format!("{} layouts", s.layouts.len()))
        }
        "layout-new" => {
            let key = args.first().ok_or("usage: layout-new <shape> [name]")?;
            let shape = Shape::from_key(key).ok_or("unknown shape")?;
            let name = args[1..].join(" ");
            store.update(Change::Layout, |s| {
                s.add_layout(&name, shape, true);
            });
            store.emit(&Change::Slots);
            Ok(store
                .get()
                .layout()
                .map(|l| l.name.clone())
                .unwrap_or_default())
        }
        "layout-use" => {
            let which = args.first().ok_or("usage: layout-use <index|name>")?;
            let index = {
                let s = store.get();
                match which.parse::<usize>() {
                    Ok(i) if i < s.layouts.len() => i,
                    _ => s
                        .layouts
                        .iter()
                        .position(|l| l.name == *which)
                        .ok_or("no such layout")?,
                }
            };
            store.update(Change::Layout, |s| s.activate_layout(index));
            store.emit(&Change::Slots);
            Ok(String::new())
        }
        "layout-rename" => {
            let name = args.join(" ");
            let mut ok = false;
            store.update(Change::Layout, |s| ok = s.rename_layout(&name));
            ok.then(String::new).ok_or("name empty or taken".into())
        }
        "layout-duplicate" => {
            store.update(Change::Layout, |s| {
                s.duplicate_layout();
            });
            store.emit(&Change::Slots);
            Ok(store
                .get()
                .layout()
                .map(|l| l.name.clone())
                .unwrap_or_default())
        }
        "layout-delete" => {
            let mut ok = false;
            store.update(Change::Layout, |s| ok = s.delete_layout());
            store.emit(&Change::Slots);
            ok.then(String::new)
                .ok_or("cannot delete the last layout".into())
        }
        "layout-clear" => {
            store.update(Change::Slots, |s| s.clear_slots());
            Ok(String::new())
        }
        "slot" | "drag" => {
            let (i, cam): (usize, i64) = match cmd {
                "slot" => (
                    args.first()
                        .and_then(|s| s.parse().ok())
                        .ok_or("usage: slot <index> <camera_id>")?,
                    args.get(1)
                        .and_then(|s| s.parse().ok())
                        .ok_or("usage: slot <index> <camera_id>")?,
                ),
                _ => (
                    args.get(1)
                        .and_then(|s| s.parse().ok())
                        .ok_or("usage: drag <camera_id> <slot>")?,
                    args.first()
                        .and_then(|s| s.parse().ok())
                        .ok_or("usage: drag <camera_id> <slot>")?,
                ),
            };
            let n = store.get().slots().len();
            if i >= n {
                return Err(format!("slot index out of range (layout has {n})"));
            }
            store.update(Change::Slots, |s| {
                s.place_camera(i, cam);
                s.selected_slot = None;
            });
            store.emit(&Change::SelectedSlot);
            Ok(String::new())
        }
        "swap" => {
            let a: usize = args
                .first()
                .and_then(|s| s.parse().ok())
                .ok_or("usage: swap <slot> <slot>")?;
            let b: usize = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .ok_or("usage: swap <slot> <slot>")?;
            let mut ok = false;
            store.update(Change::Slots, |s| ok = s.swap_slots(a, b));
            ok.then(String::new).ok_or("slot index out of range".into())
        }
        "reorder" => {
            let id: i64 = args
                .first()
                .and_then(|s| s.parse().ok())
                .ok_or("usage: reorder <camera_id> <before_camera_id|end>")?;
            let before = match args.get(1).copied() {
                Some("end") => None,
                Some(b) => Some(b.parse::<i64>().map_err(|_| "bad camera id")?),
                None => return Err("usage: reorder <camera_id> <before_camera_id|end>".into()),
            };
            store.update(Change::CameraOrder, |s| s.move_camera(id, before));
            let ids: Vec<String> = store
                .get()
                .ordered_cameras()
                .iter()
                .map(|c| c.id.to_string())
                .collect();
            Ok(ids.join(" "))
        }
        "select" => {
            let v = args.first().ok_or("usage: select <index|none>")?;
            let sel = if *v == "none" {
                None
            } else {
                Some(v.parse::<usize>().map_err(|_| "bad index")?)
            };
            store.update(Change::SelectedSlot, |s| s.selected_slot = sel);
            Ok(String::new())
        }
        "focus" => {
            let v = args.first().ok_or("usage: focus <index|none>")?;
            if *v == "none" {
                store.update(Change::FocusedSlot, |s| s.focused_slot = None);
            } else {
                let i = v.parse::<usize>().map_err(|_| "bad index")?;
                store.update(Change::FocusedSlot, |s| {
                    if s.focused_slot != Some(i) {
                        s.toggle_focus(i);
                    }
                });
            }
            store.emit(&Change::SelectedSlot);
            Ok(format!("{:?}", store.get().focused_slot))
        }
        "theme" => {
            let t = match args.first().copied() {
                Some("dark") => Theme::Dark,
                Some("light") => Theme::Light,
                _ => return Err("usage: theme <dark|light>".into()),
            };
            store.update(Change::Theme, |s| s.theme = t);
            Ok(String::new())
        }
        "panel" => {
            let on = on_off(args.first().copied())?;
            store.update(Change::PanelVisible, |s| s.panel_visible = on);
            Ok(String::new())
        }
        "timeline" => {
            let on = on_off(args.first().copied())?;
            store.update(Change::TimelineVisible, |s| s.timeline_visible = on);
            Ok(String::new())
        }
        "click" => {
            let name = args.first().ok_or("usage: click <widget-name>")?;
            let w = find_named(name).ok_or_else(|| format!("no widget named {name}"))?;
            activate(&w)
        }
        "type" => {
            let name = args.first().ok_or("usage: type <widget-name> <text>")?;
            let text = args[1..].join(" ");
            let w = find_named(name).ok_or_else(|| format!("no widget named {name}"))?;
            if let Some(e) = w.downcast_ref::<gtk::Editable>() {
                e.set_text(&text);
                Ok(String::new())
            } else if let Ok(e) = w.clone().downcast::<gtk::Editable>() {
                e.set_text(&text);
                Ok(String::new())
            } else {
                Err(format!("{name} is not editable ({})", w.type_().name()))
            }
        }
        "show" | "hide" => {
            let name = args.first().ok_or("usage: show|hide <widget-name>")?;
            let w = find_named(name).ok_or_else(|| format!("no widget named {name}"))?;
            w.set_visible(cmd == "show");
            Ok(String::new())
        }
        "player" => {
            let camera: Option<i64> = args.first().and_then(|s| s.parse().ok());
            let client = services.client().ok_or("not connected")?;
            let window = app.active_window().ok_or("no window")?;
            let mut filter = match camera {
                Some(id) => ss_api::RecordingFilter::camera(id),
                None => ss_api::RecordingFilter::default(),
            };
            filter = filter.page(0, 1);
            let page = services
                .run(async move { client.list_recordings(&filter).await })
                .await
                .map_err(|e| e.to_string())?;
            let rec = page.items.into_iter().next().ok_or("no recordings")?;
            let id = rec.id;
            crate::dialogs::player::open(&window, store, services, rec, None);
            Ok(format!("recording {id}"))
        }
        "ptzwin" => {
            let id: i64 = args
                .first()
                .and_then(|s| s.parse().ok())
                .ok_or("usage: ptzwin <camera_id>")?;
            let store_c = store.clone();
            let camera: crate::pages::live::ptz::CameraSource =
                Rc::new(move || store_c.get().camera(id).cloned());
            let pad = crate::pages::live::ptz::build(store, services, 99, camera, None);
            let window = gtk::Window::builder()
                .transient_for(&app.active_window().ok_or("no window")?)
                .decorated(false)
                .resizable(false)
                .build();
            window.add_css_class("ptz-pop");
            window.add_css_class("ptz-win");
            if let Some(main) = app.active_window() {
                for class in ["dark", "light"] {
                    if main.has_css_class(class) {
                        window.add_css_class(class);
                    }
                }
            }
            window.set_child(Some(&pad.root));
            window.present();
            (pad.refresh)();
            Ok(String::new())
        }
        "release" => {
            let version = args.first().ok_or("usage: release <version> [url]")?;
            let url = args
                .get(1)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}/releases", env!("CARGO_PKG_REPOSITORY")));
            crate::update_check::announce(
                store,
                crate::update_check::Release {
                    version: version.trim_start_matches('v').to_string(),
                    url,
                },
            );
            Ok(String::new())
        }
        "names" => {
            let mut out = Vec::new();
            for w in all_widgets() {
                let name = w.widget_name();
                if !name.is_empty() && name != w.type_().name() {
                    out.push(format!(
                        "  {name} ({}, {})",
                        w.type_().name(),
                        if w.is_visible() { "visible" } else { "hidden" }
                    ));
                }
            }
            println!("{}", out.join("\n"));
            Ok(format!("{} named widgets", out.len()))
        }
        "sleep" => {
            let ms: u64 = args
                .first()
                .and_then(|s| s.parse().ok())
                .ok_or("usage: sleep <ms>")?;
            glib::timeout_future(std::time::Duration::from_millis(ms)).await;
            Ok(String::new())
        }
        "shotw" => {
            let name = args
                .first()
                .ok_or("usage: shotw <widget-name> <path.png>")?;
            let path = args.get(1).ok_or("usage: shotw <widget-name> <path.png>")?;
            let w = find_named(name).ok_or_else(|| format!("no widget named {name}"))?;
            let mut last = Err(String::new());
            for _ in 0..12 {
                w.queue_draw();
                glib::timeout_future(std::time::Duration::from_millis(200)).await;
                last = render_widget(&w, path);
                if last.is_ok() {
                    break;
                }
            }
            last
        }
        "quit" => {
            app.quit();
            Ok(String::new())
        }
        other => match crate::pages::live::timeline::driver_command(other, &args) {
            Some(r) => r,
            None => Err("unknown command".into()),
        },
    }
}

fn on_off(v: Option<&str>) -> Result<bool, String> {
    match v {
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        _ => Err("expected on|off".into()),
    }
}

/// Visible toplevel windows, main window first.
fn toplevels() -> Vec<gtk::Window> {
    let model = gtk::Window::list_toplevels();
    let mut v: Vec<gtk::Window> = model
        .into_iter()
        .filter_map(|o| o.downcast::<gtk::Window>().ok())
        .filter(|w| w.is_visible())
        .collect();
    v.sort_by_key(|w| {
        if w.is::<gtk::ApplicationWindow>() {
            0
        } else {
            1
        }
    });
    v
}

fn shot(path: &str) -> Result<String, String> {
    let windows = toplevels();
    if windows.is_empty() {
        return Err("no visible window".into());
    }
    let mut written = Vec::new();
    for (i, win) in windows.iter().enumerate() {
        let target = if i == 0 {
            path.to_string()
        } else {
            match path.rsplit_once('.') {
                Some((stem, ext)) => format!("{stem}-{}.{ext}", i + 1),
                None => format!("{path}-{}", i + 1),
            }
        };
        render_window(win, &target)?;
        written.push(format!("{target} {}x{}", win.width(), win.height()));
    }
    // popovers are separate surfaces: render each open one on its own
    for (i, pop) in open_popovers().iter().enumerate() {
        let suffix = if i == 0 {
            "-popover".to_string()
        } else {
            format!("-popover{}", i + 1)
        };
        let target = match path.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem}{suffix}.{ext}"),
            None => format!("{path}{suffix}"),
        };
        if let Err(e) = render_popover(pop, &target) {
            written.push(format!("{target} ({e})"));
            continue;
        }
        written.push(format!("{target} {}x{}", pop.width(), pop.height()));
    }
    Ok(written.join(", "))
}

fn open_popovers() -> Vec<gtk::Popover> {
    all_widgets()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Popover>().ok())
        .filter(|p| p.is_visible())
        .collect()
}

fn render_popover(pop: &gtk::Popover, path: &str) -> Result<(), String> {
    let (w, h) = (pop.width(), pop.height());
    if w <= 0 || h <= 0 {
        return Err(format!(
            "popover has no size yet (mapped={}, realized={})",
            pop.is_mapped(),
            pop.is_realized()
        ));
    }
    let renderer = pop
        .renderer()
        .or_else(|| {
            pop.root()
                .and_downcast::<gtk::Window>()
                .and_then(|win| win.renderer())
        })
        .ok_or("popover has no renderer")?;
    let paintable = gtk::WidgetPaintable::new(Some(pop));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let node = snapshot.to_node().ok_or("nothing to render")?;
    let texture = renderer.render_texture(&node, None);
    texture.save_to_png(path).map_err(|e| e.to_string())
}

fn render_window(win: &gtk::Window, path: &str) -> Result<(), String> {
    let (w, h) = (win.width(), win.height());
    if w <= 0 || h <= 0 {
        return Err("window has no size yet".into());
    }
    // The paintable serves the render node of the last drawn frame. A
    // window presented between frames (dialogs on Broadway without a
    // browser never get one) has none, so fall back to snapshotting its
    // child directly, which builds the node on demand once allocated.
    let paintable = gtk::WidgetPaintable::new(Some(win));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let node = match snapshot.to_node() {
        Some(n) => n,
        None => {
            let child = win.child().ok_or("window has no child")?;
            win.allocate(w, h, -1, None);
            let snapshot = gtk::Snapshot::new();
            win.snapshot_child(&child, &snapshot);
            snapshot.to_node().ok_or_else(|| {
                format!(
                    "nothing to render (mapped={}, realized={})",
                    win.is_mapped(),
                    win.is_realized()
                )
            })?
        }
    };
    let renderer = win
        .renderer()
        .or_else(|| toplevels().into_iter().find_map(|t| t.renderer()))
        .ok_or("window has no renderer")?;
    let texture = renderer.render_texture(&node, None);
    texture.save_to_png(path).map_err(|e| e.to_string())
}

fn render_widget(w: &gtk::Widget, path: &str) -> Result<String, String> {
    let (width, height) = (w.width(), w.height());
    if width <= 0 || height <= 0 {
        return Err("widget has no size yet".into());
    }
    let paintable = gtk::WidgetPaintable::new(Some(w));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, width as f64, height as f64);
    let node = snapshot.to_node().ok_or("nothing to render")?;
    let renderer = w
        .root()
        .and_then(|r| r.downcast::<gtk::Window>().ok())
        .and_then(|win| win.renderer())
        .or_else(|| w.native().and_then(|n| n.renderer()))
        .ok_or("no renderer")?;
    let texture = renderer.render_texture(&node, None);
    texture.save_to_png(path).map_err(|e| e.to_string())?;
    Ok(format!("{path} {width}x{height}"))
}

fn all_widgets() -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    for win in gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|o| o.downcast::<gtk::Widget>().ok())
    {
        walk(&win, &mut out);
    }
    out
}

fn walk(w: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
    out.push(w.clone());
    let mut child = w.first_child();
    while let Some(c) = child {
        walk(&c, out);
        child = c.next_sibling();
    }
}

fn find_named(name: &str) -> Option<gtk::Widget> {
    all_widgets().into_iter().find(|w| w.widget_name() == name)
}

fn activate(w: &gtk::Widget) -> Result<String, String> {
    if let Some(b) = w.downcast_ref::<gtk::MenuButton>() {
        if b.is_active() {
            b.popdown();
        } else {
            b.popup();
        }
        return Ok("menu".into());
    }
    if let Some(b) = w.downcast_ref::<gtk::ToggleButton>() {
        b.set_active(!b.is_active());
        return Ok(format!("active={}", b.is_active()));
    }
    if let Some(b) = w.downcast_ref::<gtk::CheckButton>() {
        b.set_active(!b.is_active());
        return Ok(format!("active={}", b.is_active()));
    }
    if let Some(b) = w.downcast_ref::<gtk::Switch>() {
        b.set_active(!b.is_active());
        return Ok(format!("active={}", b.is_active()));
    }
    if let Some(b) = w.downcast_ref::<gtk::Button>() {
        if !b.is_sensitive() {
            return Err("button is insensitive".into());
        }
        b.emit_clicked();
        return Ok(String::new());
    }
    if w.activate() {
        return Ok("activated".into());
    }
    Err(format!("don't know how to activate a {}", w.type_().name()))
}
