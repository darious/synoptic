//! Application state, change notifications and persistence.
//!
//! One [`Store`] per window. Widgets read with [`Store::get`], mutate with
//! [`Store::update`] (which emits a [`Change`]) and react with
//! [`Store::subscribe`]. Fields follow the "State" section of
//! the UI spec. Persistent fields are mirrored into
//! `ss_config::AppConfig` and saved (debounced) through `ss_config::save`.

use std::cell::{Cell, Ref, RefCell, RefMut};
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

/// Live View grid shape: `rows x cols` presets plus the two Synology
/// "one big + small" shapes. Keys match `ss_config::LAYOUT_SHAPES`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Shape {
    S1x1,
    S1x2,
    S2x1,
    S2x2,
    S2x3,
    S3x2,
    S3x3,
    S3x4,
    S4x3,
    S4x4,
    /// 3×3 grid, slot 0 spans the top-left 2×2.
    Big1p5,
    /// 4×4 grid, slot 0 spans the top-left 3×3.
    Big1p7,
}

/// A cell in the `gtk::Grid`: column, row, column span, row span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridCell {
    pub col: i32,
    pub row: i32,
    pub colspan: i32,
    pub rowspan: i32,
}

impl Shape {
    pub const ALL: [Shape; 12] = [
        Shape::S1x1,
        Shape::S1x2,
        Shape::S2x1,
        Shape::S2x2,
        Shape::S2x3,
        Shape::S3x2,
        Shape::S3x3,
        Shape::S3x4,
        Shape::S4x3,
        Shape::S4x4,
        Shape::Big1p5,
        Shape::Big1p7,
    ];

    /// `"2x3"`, `"1+5"` — also the `config.toml` key.
    pub fn key(self) -> &'static str {
        match self {
            Shape::S1x1 => "1x1",
            Shape::S1x2 => "1x2",
            Shape::S2x1 => "2x1",
            Shape::S2x2 => "2x2",
            Shape::S2x3 => "2x3",
            Shape::S3x2 => "3x2",
            Shape::S3x3 => "3x3",
            Shape::S3x4 => "3x4",
            Shape::S4x3 => "4x3",
            Shape::S4x4 => "4x4",
            Shape::Big1p5 => "1+5",
            Shape::Big1p7 => "1+7",
        }
    }

    pub fn from_key(key: &str) -> Option<Shape> {
        Shape::ALL.into_iter().find(|l| l.key() == key)
    }

    /// Grid rows and columns (the big shapes are 3×3 and 4×4 grids).
    pub fn dims(self) -> (usize, usize) {
        match self {
            Shape::S1x1 => (1, 1),
            Shape::S1x2 => (1, 2),
            Shape::S2x1 => (2, 1),
            Shape::S2x2 => (2, 2),
            Shape::S2x3 => (2, 3),
            Shape::S3x2 => (3, 2),
            Shape::S3x3 | Shape::Big1p5 => (3, 3),
            Shape::S3x4 => (3, 4),
            Shape::S4x3 => (4, 3),
            Shape::S4x4 | Shape::Big1p7 => (4, 4),
        }
    }

    pub fn rows(self) -> usize {
        self.dims().0
    }

    pub fn cols(self) -> usize {
        self.dims().1
    }

    /// Number of slots.
    pub fn slots(self) -> usize {
        match self {
            Shape::Big1p5 => 6,
            Shape::Big1p7 => 8,
            _ => self.rows() * self.cols(),
        }
    }

    /// Side of the big cell in a `1+n` shape (0 for plain grids).
    fn big(self) -> usize {
        match self {
            Shape::Big1p5 => 2,
            Shape::Big1p7 => 3,
            _ => 0,
        }
    }

    /// Grid cell of slot `index` (row-major for plain grids; for `1+n`
    /// slot 0 is the big cell, then the right column top→bottom, then the
    /// bottom row left→right).
    pub fn cell(self, index: usize) -> GridCell {
        let (rows, cols) = self.dims();
        let big = self.big();
        if big == 0 {
            let cols = cols.max(1);
            return GridCell {
                col: (index % cols) as i32,
                row: (index / cols) as i32,
                colspan: 1,
                rowspan: 1,
            };
        }
        if index == 0 {
            return GridCell {
                col: 0,
                row: 0,
                colspan: big as i32,
                rowspan: big as i32,
            };
        }
        let i = index - 1;
        let (col, row) = if i < rows {
            (cols - 1, i)
        } else {
            (i - rows, rows - 1)
        };
        GridCell {
            col: col as i32,
            row: row as i32,
            colspan: 1,
            rowspan: 1,
        }
    }

    /// Material icon used in menus.
    pub fn icon(self) -> &'static str {
        match self {
            Shape::S1x1 => "crop_square",
            Shape::S1x2 | Shape::S2x1 | Shape::S2x2 => "grid_view",
            Shape::S2x3 | Shape::S3x2 | Shape::S3x3 => "apps",
            Shape::Big1p5 | Shape::Big1p7 => "open_in_full",
            _ => "view_comfy",
        }
    }

    /// Default shape for `n` cameras: 2×3 up to six, else 3×3.
    pub fn default_for(camera_count: usize) -> Shape {
        if camera_count <= 6 {
            Shape::S2x3
        } else {
            Shape::S3x3
        }
    }
}

/// One saved Live View layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutDef {
    pub name: String,
    pub shape: Shape,
    /// Camera id per slot (0 = empty), exactly `shape.slots()` long.
    pub cameras: Vec<i64>,
}

impl LayoutDef {
    pub fn new(name: impl Into<String>, shape: Shape) -> LayoutDef {
        LayoutDef {
            name: name.into(),
            shape,
            cameras: vec![0; shape.slots()],
        }
    }

    /// Change the shape, keeping cameras in slot order (truncate or pad).
    pub fn set_shape(&mut self, shape: Shape) {
        self.shape = shape;
        self.cameras.resize(shape.slots(), 0);
    }

    pub fn filled(&self) -> usize {
        self.cameras.iter().filter(|&&c| c != 0).count()
    }

    /// Fill empty slots with `cameras` (in order) that are not placed yet.
    pub fn auto_fill(&mut self, cameras: &[i64]) {
        let placed = self.cameras.clone();
        let mut fresh = cameras.iter().filter(|c| !placed.contains(c));
        for slot in self.cameras.iter_mut() {
            if *slot != 0 {
                continue;
            }
            match fresh.next() {
                Some(&c) => *slot = c,
                None => break,
            }
        }
    }

    fn from_config(l: &ss_config::LayoutDef) -> Option<LayoutDef> {
        let shape = Shape::from_key(&l.shape)?;
        let mut cameras: Vec<i64> = l.cameras.iter().map(|&c| i64::from(c)).collect();
        cameras.resize(shape.slots(), 0);
        Some(LayoutDef {
            name: l.name.clone(),
            shape,
            cameras,
        })
    }

    fn to_config(&self) -> ss_config::LayoutDef {
        ss_config::LayoutDef::new(
            self.name.clone(),
            self.shape.key(),
            self.cameras
                .iter()
                .map(|&c| u32::try_from(c).unwrap_or(0))
                .collect(),
        )
    }
}

/// Order `ids` by `order` (ids missing from `order` follow, in their own
/// order). Returns indices into `ids`.
pub fn order_indices(ids: &[i64], order: &[i64]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::with_capacity(ids.len());
    for o in order {
        if let Some(i) = ids.iter().position(|id| id == o)
            && !out.contains(&i)
        {
            out.push(i);
        }
    }
    for i in 0..ids.len() {
        if !out.contains(&i) {
            out.push(i);
        }
    }
    out
}

/// Move `id` in `order` so it sits before `before` (`None` = at the end).
/// `all` supplies ids not yet in `order`, so a never-reordered list has a
/// well-defined position for everything.
pub fn move_before(order: &mut Vec<i64>, all: &[i64], id: i64, before: Option<i64>) {
    let mut full: Vec<i64> = order_indices(all, order)
        .into_iter()
        .map(|i| all[i])
        .collect();
    for &o in order.iter() {
        if !full.contains(&o) {
            full.push(o);
        }
    }
    full.retain(|&c| c != id);
    let at = before
        .and_then(|b| full.iter().position(|&c| c == b))
        .unwrap_or(full.len());
    full.insert(at, id);
    *order = full;
}

/// A profile's layouts, active index and camera order from the config,
/// migrating the legacy `layout_cameras` / `session.grid_layout` pair and
/// creating a "Default" layout (2×3 up to six cameras, else 3×3, filled in
/// panel order) when there is none.
pub fn profile_layouts(
    config: &mut ss_config::AppConfig,
    profile: &str,
    cameras: &[Camera],
) -> (Vec<LayoutDef>, usize, Vec<i64>) {
    let grid_layout = config.session.grid_layout.clone();
    let state = config.profiles.get_mut(profile).map(|p| &mut p.state);
    let (mut layouts, active_name, order): (Vec<LayoutDef>, String, Vec<i64>) = match state {
        Some(st) => {
            if st.migrate_layouts(&grid_layout) {
                tracing::info!(profile, "migrated layout_cameras into named layouts");
            }
            (
                st.layouts
                    .iter()
                    .filter_map(LayoutDef::from_config)
                    .collect(),
                st.active_layout.clone(),
                st.camera_order.iter().map(|&c| i64::from(c)).collect(),
            )
        }
        None => (Vec::new(), String::new(), Vec::new()),
    };
    if layouts.is_empty() {
        let ids: Vec<i64> = cameras.iter().map(|c| c.id).collect();
        let ids: Vec<i64> = order_indices(&ids, &order)
            .into_iter()
            .map(|i| ids[i])
            .collect();
        let enabled = cameras
            .iter()
            .filter(|c| c.status == ss_api::CameraStatus::Enabled)
            .count();
        let mut l = LayoutDef::new("Default", Shape::default_for(enabled.max(ids.len().min(6))));
        l.auto_fill(&ids);
        layouts.push(l);
    }
    let active = layouts
        .iter()
        .position(|l| l.name == active_name)
        .unwrap_or(0);
    (layouts, active, order)
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
    /// Saved layouts of the current profile, in menu order (never empty
    /// while logged in).
    pub layouts: Vec<LayoutDef>,
    /// Index into `layouts`.
    pub active_layout: usize,
    /// Camera panel order (camera ids); cameras not listed follow in NAS order.
    pub camera_order: Vec<i64>,
    pub selected_slot: Option<usize>,
    /// Live View: slot shown alone (double-click / "Fill view"); the other
    /// slots stay mounted but hidden so the layout comes back instantly.
    /// Runtime only, cleared by any layout change.
    pub focused_slot: Option<usize>,
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

    /// The active layout, if any.
    pub fn layout(&self) -> Option<&LayoutDef> {
        self.layouts.get(self.active_layout)
    }

    pub fn layout_mut(&mut self) -> Option<&mut LayoutDef> {
        self.layouts.get_mut(self.active_layout)
    }

    /// Shape of the active layout (2×2 when there is none yet).
    pub fn shape(&self) -> Shape {
        self.layout().map(|l| l.shape).unwrap_or(Shape::S2x2)
    }

    /// Camera id per slot of the active layout, `shape().slots()` long.
    pub fn slots(&self) -> Vec<i64> {
        let mut v = self.layout().map(|l| l.cameras.clone()).unwrap_or_default();
        v.resize(self.shape().slots(), 0);
        v
    }

    /// Slot index (0-based) holding `camera_id` in the current layout.
    pub fn slot_of(&self, camera_id: i64) -> Option<usize> {
        self.slots().iter().position(|&c| c == camera_id)
    }

    /// Put `camera_id` (0 = empty) into `slot`; a camera lives in one slot
    /// at a time, so its previous slot becomes empty. Creates the layout
    /// when none exists. Returns false when `slot` is out of range.
    pub fn place_camera(&mut self, slot: usize, camera_id: i64) -> bool {
        self.ensure_layout();
        let Some(l) = self.layout_mut() else {
            return false;
        };
        if slot >= l.cameras.len() {
            return false;
        }
        if camera_id != 0 {
            for c in l.cameras.iter_mut() {
                if *c == camera_id {
                    *c = 0;
                }
            }
        }
        l.cameras[slot] = camera_id;
        true
    }

    /// Swap the contents of two slots of the active layout.
    pub fn swap_slots(&mut self, a: usize, b: usize) -> bool {
        let Some(l) = self.layout_mut() else {
            return false;
        };
        if a >= l.cameras.len() || b >= l.cameras.len() {
            return false;
        }
        l.cameras.swap(a, b);
        true
    }

    /// Double-click / "Fill view": show `index` alone, or back to the layout
    /// when it is already focused. Focusing also selects the slot.
    pub fn toggle_focus(&mut self, index: usize) {
        if self.focused_slot == Some(index) {
            self.focused_slot = None;
        } else if index < self.shape().slots() {
            self.focused_slot = Some(index);
            self.selected_slot = Some(index);
        }
    }

    /// Change the active layout's shape, keeping cameras in slot order.
    pub fn set_shape(&mut self, shape: Shape) {
        self.ensure_layout();
        if let Some(l) = self.layout_mut() {
            l.set_shape(shape);
        }
        self.selected_slot = None;
        self.focused_slot = None;
    }

    /// Add a layout after the active one and make it active. The name is
    /// made unique (`name 2`, …). Empty slots are auto-filled in panel order.
    pub fn add_layout(&mut self, name: &str, shape: Shape, auto_fill: bool) -> usize {
        let name = self.unique_layout_name(name.trim(), shape);
        let mut l = LayoutDef::new(name, shape);
        if auto_fill {
            let ids: Vec<i64> = self.ordered_cameras().iter().map(|c| c.id).collect();
            l.auto_fill(&ids);
        }
        let at = (self.active_layout + 1).min(self.layouts.len());
        self.layouts.insert(at, l);
        self.active_layout = at;
        self.selected_slot = None;
        self.focused_slot = None;
        at
    }

    /// Copy the active layout (`<name> copy`) after it and make it active.
    pub fn duplicate_layout(&mut self) -> Option<usize> {
        let src = self.layout()?.clone();
        let name = self.unique_layout_name(&format!("{} copy", src.name), src.shape);
        let at = self.active_layout + 1;
        self.layouts.insert(
            at,
            LayoutDef {
                name,
                shape: src.shape,
                cameras: src.cameras,
            },
        );
        self.active_layout = at;
        self.selected_slot = None;
        self.focused_slot = None;
        Some(at)
    }

    /// Delete the active layout; refused when it is the last one.
    pub fn delete_layout(&mut self) -> bool {
        if self.layouts.len() <= 1 || self.active_layout >= self.layouts.len() {
            return false;
        }
        self.layouts.remove(self.active_layout);
        self.active_layout = self.active_layout.min(self.layouts.len() - 1);
        self.selected_slot = None;
        self.focused_slot = None;
        true
    }

    pub fn rename_layout(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let taken = self
            .layouts
            .iter()
            .enumerate()
            .any(|(i, l)| i != self.active_layout && l.name == name);
        if taken {
            return false;
        }
        match self.layout_mut() {
            Some(l) => {
                l.name = name.to_string();
                true
            }
            None => false,
        }
    }

    /// Empty every slot of the active layout.
    pub fn clear_slots(&mut self) {
        if let Some(l) = self.layout_mut() {
            l.cameras.iter_mut().for_each(|c| *c = 0);
        }
        self.selected_slot = None;
        self.focused_slot = None;
    }

    /// Switch to layout `index` (clamped).
    pub fn activate_layout(&mut self, index: usize) {
        if !self.layouts.is_empty() {
            self.active_layout = index.min(self.layouts.len() - 1);
        }
        self.selected_slot = None;
        self.focused_slot = None;
    }

    fn ensure_layout(&mut self) {
        if self.layouts.is_empty() {
            let n = self.cameras.len();
            self.layouts
                .push(LayoutDef::new("Default", Shape::default_for(n)));
            self.active_layout = 0;
        }
    }

    fn unique_layout_name(&self, name: &str, shape: Shape) -> String {
        let base = if name.is_empty() { shape.key() } else { name };
        let mut candidate = base.to_string();
        let mut n = 2;
        while self.layouts.iter().any(|l| l.name == candidate) {
            candidate = format!("{base} {n}");
            n += 1;
        }
        candidate
    }

    /// Cameras in panel order: `camera_order` first, the rest in NAS order.
    pub fn ordered_cameras(&self) -> Vec<&Camera> {
        let ids: Vec<i64> = self.cameras.iter().map(|c| c.id).collect();
        order_indices(&ids, &self.camera_order)
            .into_iter()
            .map(|i| &self.cameras[i])
            .collect()
    }

    /// Move camera `id` before camera `before` (`None` = to the end) in the
    /// panel order.
    pub fn move_camera(&mut self, id: i64, before: Option<i64>) {
        let all: Vec<i64> = self.cameras.iter().map(|c| c.id).collect();
        move_before(&mut self.camera_order, &all, id, before);
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
    /// Active layout, its shape, or the layout list changed.
    Layout,
    /// Cameras in the active layout's slots changed.
    Slots,
    /// Camera panel order changed.
    CameraOrder,
    SelectedSlot,
    HomeMode,
    Notifications,
    UpdateNotice,
    FilterCamera,
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
    /// Live View: `focused_slot` changed.
    FocusedSlot,
    /// Live View: show the timeline at `(unix seconds, camera id)` and
    /// seek every slot there (from the Events page).
    RequestSeek(i64, i64),
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
        let page = Page::from_key(&config.session.last_page).unwrap_or(Page::Live);

        let state = AppState {
            page,
            theme,
            panel_visible: config.general.sidebar_visible,
            timeline_visible: config.general.timeline_visible,
            connection: None,
            cameras: Vec::new(),
            layouts: Vec::new(),
            active_layout: 0,
            camera_order: Vec::new(),
            selected_slot: None,
            focused_slot: None,
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

    /// Direct mutable access without notification or persistence. Prefer
    /// [`Store::update`]; this exists for reverting optimistic changes.
    pub fn state_mut(&self) -> RefMut<'_, AppState> {
        self.state.borrow_mut()
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
            Change::Layout | Change::Slots | Change::CameraOrder => {
                c.session.grid_layout = s.shape().key().to_string();
                let name = s
                    .connection
                    .as_ref()
                    .map(|c| c.profile.clone())
                    .unwrap_or_else(|| c.general.default_profile.clone());
                if let Some(p) = c.profiles.get_mut(&name) {
                    p.state.layouts = s.layouts.iter().map(LayoutDef::to_config).collect();
                    p.state.active_layout = s.layout().map(|l| l.name.clone()).unwrap_or_default();
                    p.state.camera_order = s
                        .camera_order
                        .iter()
                        .filter_map(|&c| u32::try_from(c).ok())
                        .collect();
                    // keep the reference client's square layouts in step
                    for l in &s.layouts {
                        if ss_config::GRID_LAYOUTS.contains(&l.shape.key()) {
                            p.state.layout_cameras.insert(
                                l.shape.key().to_string(),
                                l.cameras.iter().map(|&c| c.max(0) as u32).collect(),
                            );
                        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use ss_api::{CameraStatus, VideoCodec};

    fn cam(id: i64) -> Camera {
        Camera {
            id,
            name: format!("Cam {id}"),
            model: String::new(),
            vendor: String::new(),
            status: CameraStatus::Enabled,
            is_ptz: false,
            has_audio: false,
            has_speaker: false,
            def_live_profile: None,
            video_codec: VideoCodec::H264,
        }
    }

    fn state(n_cams: usize) -> AppState {
        AppState {
            page: Page::Live,
            theme: Theme::Dark,
            panel_visible: true,
            timeline_visible: true,
            connection: None,
            cameras: (1..=n_cams as i64).map(cam).collect(),
            layouts: Vec::new(),
            active_layout: 0,
            camera_order: Vec::new(),
            selected_slot: None,
            focused_slot: None,
            home_mode: false,
            unread_notifications: 0,
            update_notice_unread: false,
            filter_camera: None,
            rail_labels: false,
            density_compact: false,
        }
    }

    fn c(col: i32, row: i32, colspan: i32, rowspan: i32) -> GridCell {
        GridCell {
            col,
            row,
            colspan,
            rowspan,
        }
    }

    #[test]
    fn keys_round_trip_and_match_config() {
        for shape in Shape::ALL {
            assert_eq!(Shape::from_key(shape.key()), Some(shape));
            assert_eq!(
                ss_config::shape_slots(shape.key()),
                Some(shape.slots()),
                "{}",
                shape.key()
            );
        }
        assert_eq!(Shape::from_key("5x5"), None);
        assert_eq!(ss_config::LAYOUT_SHAPES.len(), Shape::ALL.len());
    }

    #[test]
    fn focus_toggles_selects_and_is_cleared_by_layout_changes() {
        let mut s = state(6);
        s.add_layout("Default", Shape::S2x3, true);
        s.toggle_focus(4);
        assert_eq!(s.focused_slot, Some(4));
        assert_eq!(s.selected_slot, Some(4));
        s.toggle_focus(4);
        assert_eq!(s.focused_slot, None);
        // out of range: ignored
        s.toggle_focus(6);
        assert_eq!(s.focused_slot, None);
        s.toggle_focus(1);
        s.set_shape(Shape::S3x3);
        assert_eq!(s.focused_slot, None);
        s.toggle_focus(1);
        s.add_layout("Other", Shape::S1x1, true);
        assert_eq!(s.focused_slot, None);
    }

    #[test]
    fn plain_grids_are_row_major_single_cells() {
        assert_eq!(Shape::S1x1.cell(0), c(0, 0, 1, 1));
        assert_eq!(Shape::S2x3.dims(), (2, 3));
        assert_eq!(Shape::S2x3.slots(), 6);
        assert_eq!(Shape::S2x3.cell(2), c(2, 0, 1, 1));
        assert_eq!(Shape::S2x3.cell(3), c(0, 1, 1, 1));
        assert_eq!(Shape::S3x2.cell(2), c(0, 1, 1, 1));
        assert_eq!(Shape::S3x2.cell(5), c(1, 2, 1, 1));
        assert_eq!(Shape::S4x3.cell(11), c(2, 3, 1, 1));
        assert_eq!(Shape::S3x4.cell(11), c(3, 2, 1, 1));
        assert_eq!(Shape::S4x4.cell(15), c(3, 3, 1, 1));
    }

    #[test]
    fn big_shapes_span_and_cover_the_grid_exactly_once() {
        assert_eq!(Shape::Big1p5.cell(0), c(0, 0, 2, 2));
        assert_eq!(Shape::Big1p5.cell(1), c(2, 0, 1, 1));
        assert_eq!(Shape::Big1p5.cell(3), c(2, 2, 1, 1));
        assert_eq!(Shape::Big1p5.cell(4), c(0, 2, 1, 1));
        assert_eq!(Shape::Big1p5.cell(5), c(1, 2, 1, 1));
        assert_eq!(Shape::Big1p7.cell(0), c(0, 0, 3, 3));
        assert_eq!(Shape::Big1p7.cell(4), c(3, 3, 1, 1));
        assert_eq!(Shape::Big1p7.cell(7), c(2, 3, 1, 1));
        for shape in Shape::ALL {
            let (rows, cols) = shape.dims();
            let mut covered = vec![false; rows * cols];
            for i in 0..shape.slots() {
                let cell = shape.cell(i);
                for r in cell.row..cell.row + cell.rowspan {
                    for col in cell.col..cell.col + cell.colspan {
                        let k = r as usize * cols + col as usize;
                        assert!(!covered[k], "{} slot {i} overlaps", shape.key());
                        covered[k] = true;
                    }
                }
            }
            assert!(covered.iter().all(|&x| x), "{} leaves a hole", shape.key());
        }
    }

    #[test]
    fn set_shape_keeps_slot_order() {
        let mut l = LayoutDef::new("x", Shape::S2x2);
        l.cameras = vec![1, 2, 3, 4];
        l.set_shape(Shape::S2x3);
        assert_eq!(l.cameras, vec![1, 2, 3, 4, 0, 0]);
        l.set_shape(Shape::S1x2);
        assert_eq!(l.cameras, vec![1, 2]);
        l.auto_fill(&[2, 3, 4]);
        assert_eq!(l.cameras, vec![1, 2]);
        l.set_shape(Shape::Big1p5);
        l.auto_fill(&[2, 3, 4]);
        assert_eq!(l.cameras, vec![1, 2, 3, 4, 0, 0]);
        assert_eq!(l.filled(), 4);
    }

    #[test]
    fn ordering_puts_unknown_cameras_last_in_nas_order() {
        assert_eq!(order_indices(&[1, 2, 3, 4], &[]), vec![0, 1, 2, 3]);
        assert_eq!(
            order_indices(&[1, 2, 3, 4], &[3, 9, 1, 3]),
            vec![2, 0, 1, 3]
        );
        let mut order = Vec::new();
        move_before(&mut order, &[1, 2, 3, 4], 4, Some(2));
        assert_eq!(order, vec![1, 4, 2, 3]);
        move_before(&mut order, &[1, 2, 3, 4], 1, None);
        assert_eq!(order, vec![4, 2, 3, 1]);
        move_before(&mut order, &[1, 2, 3, 4], 1, Some(1));
        assert_eq!(order, vec![4, 2, 3, 1]);
    }

    #[test]
    fn app_state_places_swaps_and_orders() {
        let mut s = state(6);
        assert_eq!(s.shape(), Shape::S2x2, "no layouts yet");
        assert!(s.place_camera(1, 3));
        assert_eq!(s.shape(), Shape::S2x3, "default created for 6 cameras");
        assert_eq!(s.slot_of(3), Some(1));
        assert!(s.place_camera(4, 3));
        assert_eq!(s.slots(), vec![0, 0, 0, 0, 3, 0]);
        assert!(!s.place_camera(6, 1));
        assert!(s.place_camera(0, 1));
        assert!(s.swap_slots(0, 4));
        assert_eq!(s.slots()[0], 3);
        assert_eq!(s.slots()[4], 1);
        s.move_camera(5, Some(1));
        let ids: Vec<i64> = s.ordered_cameras().iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![5, 1, 2, 3, 4, 6]);
        let at = s.add_layout("", Shape::Big1p5, true);
        assert_eq!(at, 1);
        assert_eq!(s.layout().unwrap().name, "1+5");
        assert_eq!(
            s.slots(),
            vec![5, 1, 2, 3, 4, 6],
            "auto-filled in panel order"
        );
        assert!(!s.rename_layout("Default"), "names are unique");
        assert!(s.rename_layout("Front"));
        s.duplicate_layout();
        assert_eq!(s.layout().unwrap().name, "Front copy");
        assert_eq!(s.layouts.len(), 3);
        assert!(s.delete_layout());
        assert_eq!(s.active_layout, 1);
        s.set_shape(Shape::S1x1);
        assert_eq!(s.slots(), vec![5]);
        s.clear_slots();
        assert_eq!(s.slots(), vec![0]);
        s.activate_layout(0);
        assert!(s.delete_layout());
        assert!(!s.delete_layout(), "the last layout stays");
    }

    #[test]
    fn profile_layouts_create_a_default_and_migrate() {
        let cams: Vec<Camera> = (1..=7).map(cam).collect();
        let mut config = ss_config::AppConfig::default();
        config.add_profile("nas", ss_config::Profile::new("h"));
        let (layouts, active, order) = profile_layouts(&mut config, "nas", &cams);
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].shape, Shape::S3x3);
        assert_eq!(layouts[0].cameras, vec![1, 2, 3, 4, 5, 6, 7, 0, 0]);
        assert_eq!(active, 0);
        assert!(order.is_empty());
        let (layouts, ..) = profile_layouts(&mut config, "nas", &cams[..6]);
        assert_eq!(layouts[0].shape, Shape::S2x3);

        config.session.grid_layout = "3x3".into();
        let p = config.profiles.get_mut("nas").unwrap();
        p.state
            .layout_cameras
            .insert("2x2".into(), vec![1, 2, 3, 4]);
        p.state.layout_cameras.insert("3x3".into(), vec![7]);
        p.state.camera_order = vec![7, 1];
        let (layouts, active, order) = profile_layouts(&mut config, "nas", &cams);
        assert_eq!(layouts.len(), 2);
        assert_eq!(active, 1);
        assert_eq!(layouts[1].cameras.len(), 9);
        assert_eq!(order, vec![7, 1]);
        assert_eq!(
            config.profiles["nas"].state.layouts.len(),
            2,
            "migration written back"
        );
    }
}
