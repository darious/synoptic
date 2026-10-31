# ss-ui implementation plan

Spec: `design/handoff/README.md` (authoritative; the `.dc.html` files are a
JS-rendered prototype). Tokens are already in `crates/ss-ui/data/style.css`.
Icons: Material Symbols Rounded SVGs in `crates/ss-ui/data/icons/` as
`ms-<name>-symbolic` (GTK recolours `-symbolic` SVGs with the CSS `color`).

## Stack

- gtk4-rs 0.10 (GTK 4.18), plain GTK widgets + our CSS, no libadwaita.
- Video: GStreamer `appsrc ! h26xparse ! decodebin3 ! gtk4paintablesink`,
  one pipeline per slot; `gtk::Picture` shows the sink's `Paintable`.
  VAAPI through `decodebin3` auto-plugging (`vah264dec`/`vah265dec`).
- Async: one tokio runtime on a background thread (`services.rs`) owning
  `ss_api::Client`, `ss_stream::MixStreamClient`s and polling loops. UI ↔
  tokio via `async_channel` (glib main context side uses `glib::spawn_future_local`).
- State: `state.rs` holds `AppState` (fields exactly as the spec's "State"
  section) in `Rc<RefCell<_>>` plus a tiny signal bus (`Rc<Bus>` of
  `Vec<Box<dyn Fn(&Change)>>`); widgets subscribe and re-render on change.
  Persistence maps 1:1 onto `ss_config::AppConfig`.

## Module layout (one owner per module during parallel work)

```
crates/ss-ui/src
├── main.rs            gtk::Application, CSS + icon resource loading, theme class
├── state.rs           AppState, Change enum, Bus, persistence glue (ss-config)
├── services.rs        tokio runtime thread, ApiHandle (login/cameras/recordings/…), StreamHandle
├── icons.rs           const fn names: icon("videocam") -> "ms-videocam-symbolic"
├── widgets/
│   ├── header.rs      48px header: logo, panel toggle, Home Mode, title, page controls, bell, theme, avatar/Login
│   ├── rail.rs        56px rail, active indicator, disabled-when-logged-out, About dot
│   ├── camera_panel.rs 240px panel: header/count/hint, All cameras row, camera rows, Empty slot, footer
│   ├── toast.rs       overlay toast, 2.2s
│   ├── popovers.rs    layout menu, notifications, account menu
│   └── controls.rs    chip, spinner box, toggle row, status dot, pills (small reusable builders)
├── pages/
│   ├── mod.rs         Page enum, gtk::Stack wiring
│   ├── live/
│   │   ├── grid.rs    n×n grid on black, 1px gaps, per-layout slot memory, selection
│   │   ├── slot.rs    Picture + overlays (name pill, status pill, timecode, hover toolbar, empty/offline states)
│   │   └── timeline.rs 112px strip: controls row + Cairo ruler (DrawingArea), live/history logic
│   ├── browse/
│   │   ├── filter_bar.rs quick filter pills, type pills, camera filter box, advanced row, summary row
│   │   ├── recordings.rs, events.rs, snapshots.rs, timelapse.rs
│   │   └── pagination.rs
│   ├── licenses.rs
│   ├── settings.rs
│   ├── about.rs
│   └── logged_out.rs
├── dialogs/
│   ├── login.rs, camera_settings.rs, player.rs, snapshot_viewer.rs
│   └── shell.rs       modal scrim + dialog frame (header/body/footer)
└── video/
    ├── pipeline.rs    GStreamer pipeline builder, appsrc push, codec switch, paintable
    └── bridge.rs      ss-stream Event::Frame → appsrc; history commands; stats
```

## Parallel work split (after the shell compiles)

| Agent | Owns | Depends on |
|---|---|---|
| shell (owner) | main, state, services, icons, widgets/header, rail, camera_panel, toast, pages/mod, logged_out | — |
| A live | pages/live/*, video/* | state, services |
| B browse | pages/browse/*, dialogs/player, dialogs/snapshot_viewer | state, services, dialogs/shell |
| C admin | pages/licenses, settings, about; dialogs/login, camera_settings; widgets/popovers, controls | state, services, dialogs/shell |

Shared contracts frozen by the owner before agents start: `AppState`/`Change`,
`ApiHandle` method list, `dialogs/shell.rs`, `icons.rs`, CSS class names
(already in `style.css`).

## Fonts

`fonts-ibm-plex` provides "IBM Plex Sans"/"IBM Plex Mono"; CSS already names
them with sans-serif/monospace fallbacks.

## Testing on this machine

Headless tty, no Xvfb. `cargo build -p ss-ui` and unit tests of pure logic
(timeline math, pagination, filters) run here; visual checks need a desktop
session. Keep logic out of widgets where practical so it is testable.
