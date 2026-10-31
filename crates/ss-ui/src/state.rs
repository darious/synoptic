//! Application state, change notifications and persistence.
//!
//! One [`Store`] per window. Widgets read with [`Store::get`], mutate with
//! [`Store::update`] (which emits a [`Change`]) and react with
//! [`Store::subscribe`]. Fields follow the "State" section of
//! `design/handoff/README.md`. Persistent fields are mirrored into
//! `ss_config::AppConfig` and saved (debounced) through `ss_config::save`.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::BTreeMap;
use std::rc::Rc;

use gtk::glib;
use ss_api::Camera;

/// Pages on the rail, in rail order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Page {
    Live,
    Recordings,
    Snapshots,
    Events,
    TimeLapse,
    Licenses,
    Settings,
    About,
}

impl Page {
    pub const ALL: [Page; 8] = [
        Page::Live,
        Page::Recordings,
        Page::Snapshots,
        Page::Events,
        Page::TimeLapse,
        Page::Licenses,
        Page::Settings,
        Page::About,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::Live => "Live View",
            Page::Recordings => "Recordings",
            Page::Snapshots => "Snapshots",
            Page::Events => "Events",
            Page::TimeLapse => "Time Lapse",
            Page::Licenses => "Licenses",
            Page::Settings => "Settings",
            Page::About => "About",
        }
    }

    /// Material Symbols icon name (see `icons.rs`).
    pub fn icon(self) -> &'static str {
        match self {
            Page::Live => "videocam",
            Page::Recordings => "play_circle",
            Page::Snapshots => "photo_library",
            Page::Events => "warning",
            Page::TimeLapse => "timelapse",
            Page::Licenses => "key",
            Page::Settings => "settings",
            Page::About => "info",
        }
    }

    /// Key used by `config.toml` `session.last_page` (same as the reference app).
    pub fn key(self) -> &'static str {
        match self {
            Page::Live => "live",
            Page::Recordings => "recordings",
            Page::Snapshots => "snapshots",
            Page::Events => "events",
            Page::TimeLapse => "timelapse",
            Page::Licenses => "licenses",
            Page::Settings => "settings",
            Page::About => "about",
        }
    }

    pub fn from_key(key: &str) -> Option<Page> {
        Page::ALL.into_iter().find(|p| p.key() == key)
    }

    /// Recordings / Snapshots / Events / Time Lapse share the browse chrome.
    pub fn is_browse(self) -> bool {
        matches!(
            self,
            Page::Recordings | Page::Snapshots | Page::Events | Page::TimeLapse
        )
    }

    /// Usable while logged out.
    pub fn available_offline(self) -> bool {
        matches!(self, Page::Settings | Page::About)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

/// Live View grid size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layout {
    L1,
    L2,
    L3,
    L4,
}

impl Layout {
    pub const ALL: [Layout; 4] = [Layout::L1, Layout::L2, Layout::L3, Layout::L4];

    /// Grid side length.
    pub fn n(self) -> usize {
        match self {
            Layout::L1 => 1,
            Layout::L2 => 2,
            Layout::L3 => 3,
            Layout::L4 => 4,
        }
    }

    pub fn slots(self) -> usize {
        self.n() * self.n()
    }

    /// `"2x2"` — also the `config.toml` key.
    pub fn key(self) -> &'static str {
        match self {
            Layout::L1 => "1x1",
            Layout::L2 => "2x2",
            Layout::L3 => "3x3",
            Layout::L4 => "4x4",
        }
    }

    pub fn from_key(key: &str) -> Option<Layout> {
        Layout::ALL.into_iter().find(|l| l.key() == key)
    }

    pub fn icon(self) -> &'static str {
        match self {
            Layout::L1 => "crop_square",
            Layout::L2 => "grid_view",
            Layout::L3 => "apps",
            Layout::L4 => "view_comfy",
        }
    }
}

/// The NAS we are logged in to.
#[derive(Clone, Debug, Default)]
pub struct Connection {
    pub profile: String,
    pub host: String,
    pub username: String,
    /// e.g. `9.3.1-11xxx`
    pub ss_version: String,
    pub ds_model: String,
}

/// Everything the widgets render from.
#[derive(Debug)]
pub struct AppState {
    pub page: Page,
    pub theme: Theme,
    pub panel_visible: bool,
    pub timeline_visible: bool,
    pub connection: Option<Connection>,
    pub cameras: Vec<Camera>,
    pub layout: Layout,
    /// Camera id per slot (0 = empty), per layout.
    pub slots_by_layout: BTreeMap<Layout, Vec<i64>>,
    pub selected_slot: Option<usize>,
    pub home_mode: bool,
    pub unread_notifications: u32,
    pub update_notice_unread: bool,
    /// Browse pages: camera filter (None = all cameras).
    pub filter_camera: Option<i64>,
    /// Prototype tweaks.
    pub rail_labels: bool,
    pub density_compact: bool,
}

impl AppState {
    pub fn logged_in(&self) -> bool {
        self.connection.is_some()
    }

    pub fn slots(&self) -> Vec<i64> {
        let mut v = self
            .slots_by_layout
            .get(&self.layout)
            .cloned()
            .unwrap_or_default();
        v.resize(self.layout.slots(), 0);
        v
    }

    /// Slot index (0-based) holding `camera_id` in the current layout.
    pub fn slot_of(&self, camera_id: i64) -> Option<usize> {
        self.slots().iter().position(|&c| c == camera_id)
    }

    pub fn camera(&self, id: i64) -> Option<&Camera> {
        self.cameras.iter().find(|c| c.id == id)
    }

    pub fn online_count(&self) -> usize {
        self.cameras
            .iter()
            .filter(|c| c.status == ss_api::CameraStatus::Enabled)
            .count()
    }
}

/// What changed; subscribers filter on it.
#[derive(Clone, Debug)]
pub enum Change {
    Page,
    Theme,
    PanelVisible,
    TimelineVisible,
    Connection,
    Cameras,
    Layout,
    Slots,
    SelectedSlot,
    HomeMode,
    Notifications,
    UpdateNotice,
    FilterCamera,
    Density,
    /// Show a toast (bottom centre, 2.2 s).
    Toast(String),
    /// Open the login dialog.
    RequestLogin,
    /// Sign out.
    RequestLogout,
    /// Open Camera Settings for a camera.
    RequestCameraSettings(i64),
    /// Camera Settings were applied for a camera (protocol, dewarp, …).
    CameraSettings(i64),
    /// Live View: reload every stream.
    RequestReload,
    /// Live View: open camera in a 1×1 standalone view.
    RequestOpenSingle(i64),
}

type Listener = Box<dyn Fn(&Change)>;

pub struct Store {
    state: RefCell<AppState>,
    listeners: RefCell<Vec<Listener>>,
    config: RefCell<ss_config::AppConfig>,
    save_scheduled: Cell<bool>,
}

impl Store {
    /// Load `config.toml` (defaults when absent) and build the initial state.
    pub fn load() -> Rc<Store> {
        let config = match ss_config::load() {
            Ok(loaded) => {
                if let Some(p) = &loaded.salvaged_to {
                    tracing::warn!("unreadable config moved to {}", p.display());
                }
                loaded.config
            }
            Err(e) => {
                tracing::warn!("config: {e}; using defaults");
                ss_config::AppConfig::default()
            }
        };

        let theme = match config.general.theme {
            ss_config::Theme::Light => Theme::Light,
            _ => Theme::Dark,
        };
        let layout = Layout::from_key(&config.session.grid_layout).unwrap_or(Layout::L2);
        let page = Page::from_key(&config.session.last_page).unwrap_or(Page::Live);
        let mut slots_by_layout = BTreeMap::new();
        if let Some(profile) = config.default_profile() {
            for (key, cams) in &profile.state.layout_cameras {
                if let Some(l) = Layout::from_key(key) {
                    slots_by_layout.insert(l, cams.iter().map(|&c| c as i64).collect());
                }
            }
        }

        let state = AppState {
            page,
            theme,
            panel_visible: config.general.sidebar_visible,
            timeline_visible: config.general.timeline_visible,
            connection: None,
            cameras: Vec::new(),
            layout,
            slots_by_layout,
            selected_slot: None,
            home_mode: false,
            unread_notifications: 0,
            update_notice_unread: false,
            filter_camera: None,
            rail_labels: false,
            density_compact: false,
        };

        Rc::new(Store {
            state: RefCell::new(state),
            listeners: RefCell::new(Vec::new()),
            config: RefCell::new(config),
            save_scheduled: Cell::new(false),
        })
    }

    pub fn get(&self) -> Ref<'_, AppState> {
        self.state.borrow()
    }

    /// The loaded `config.toml` model; mutate then call [`Store::save_soon`].
    pub fn config(&self) -> RefMut<'_, ss_config::AppConfig> {
        self.config.borrow_mut()
    }

    pub fn subscribe(&self, f: impl Fn(&Change) + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    /// Mutate state, mirror persistent fields into the config, notify.
    pub fn update(self: &Rc<Self>, change: Change, f: impl FnOnce(&mut AppState)) {
        {
            let mut s = self.state.borrow_mut();
            f(&mut s);
            self.mirror_to_config(&s, &change);
        }
        self.emit(&change);
    }

    /// Notify without changing state (requests, toasts).
    pub fn emit(self: &Rc<Self>, change: &Change) {
        // Listeners may subscribe/emit re-entrantly; don't hold the borrow.
        let n = self.listeners.borrow().len();
        for i in 0..n {
            let listeners = self.listeners.borrow();
            if let Some(l) = listeners.get(i) {
                let l: &Listener = l;
                // Safety of re-entrancy: we re-borrow per call and never hold
                // a `RefMut` across the call.
                let l = l as *const Listener;
                drop(listeners);
                // SAFETY: listeners are only ever appended, never removed, so
                // the boxed closure at `i` stays alive for the Store's lifetime.
                unsafe { (*l)(change) };
            }
        }
    }

    pub fn toast(self: &Rc<Self>, text: impl Into<String>) {
        self.emit(&Change::Toast(text.into()));
    }

    // --- persistence -----------------------------------------------------

    fn mirror_to_config(&self, s: &AppState, change: &Change) {
        let mut c = self.config.borrow_mut();
        match change {
            Change::Theme => {
                c.general.theme = match s.theme {
                    Theme::Dark => ss_config::Theme::Dark,
                    Theme::Light => ss_config::Theme::Light,
                }
            }
            Change::PanelVisible => c.general.sidebar_visible = s.panel_visible,
            Change::TimelineVisible => c.general.timeline_visible = s.timeline_visible,
            Change::Page => c.session.last_page = s.page.key().to_string(),
            Change::Layout => c.session.grid_layout = s.layout.key().to_string(),
            Change::Slots => {
                let name = s
                    .connection
                    .as_ref()
                    .map(|c| c.profile.clone())
                    .unwrap_or_else(|| c.general.default_profile.clone());
                if let Some(p) = c.profiles.get_mut(&name) {
                    for (layout, cams) in &s.slots_by_layout {
                        p.state.layout_cameras.insert(
                            layout.key().to_string(),
                            cams.iter().map(|&c| c.max(0) as u32).collect(),
                        );
                    }
                }
            }
            _ => return,
        }
        drop(c);
        self.schedule_save();
    }

    /// Save the config in ~500 ms unless another change comes first.
    pub fn save_soon(self: &Rc<Self>) {
        self.schedule_save();
    }

    fn schedule_save(&self) {
        if self.save_scheduled.replace(true) {
            return;
        }
        // `Store` lives as long as the window; a weak pointer is not available
        // from `&self`, so copy what we need when the timer fires via a
        // thread-local registry of pending stores.
        PENDING.with(|p| p.borrow_mut().push(self as *const Store));
        glib::timeout_add_local_once(std::time::Duration::from_millis(500), || {
            let stores = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
            for ptr in stores {
                // SAFETY: a Store is only dropped when the application quits,
                // after the main loop (and this timeout) is gone.
                let store = unsafe { &*ptr };
                store.save_scheduled.set(false);
                if let Err(e) = ss_config::save(&store.config.borrow()) {
                    tracing::warn!("saving config: {e}");
                }
            }
        });
    }
}

thread_local! {
    static PENDING: RefCell<Vec<*const Store>> = const { RefCell::new(Vec::new()) };
}
