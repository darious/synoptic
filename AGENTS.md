# Synoptic — agent map

Linux client for Synology Surveillance Station in Rust. Reverse-engineering
notes and the port plan: `docs/architecture.md` (read §5–§7 first).
Reference implementation (Python/GTK, BSD-2) is cloned at
`~/.cache/synoptic/ref/ssc-gtk`; port its *protocol knowledge*, not its UI.

## Layout

| Crate | Scope | Depends on |
|---|---|---|
| `crates/ss-api` | DSM Web API: `SYNO.API.Info` discovery, `SYNO.API.Auth` login v6 (OTP, device token), error tables, Camera/Recording/Event/Snapshot/PTZ calls, typed models | – |
| `crates/ss-stream` | `/ss_webstream_task/` MixStream WebSocket: URL builders (live `stmSrc=0`, history `stmSrc=2`), frame parser (4-byte BE header length + `k=v&…` header + payload), in-band commands (`action=play…`, `seekMs=…&stamp=n`, `stmSrc=1&start=…`), keepalive, reconnect | – (takes a session id/cookie, not `ss-api` types) |
| `crates/ss-play` | `FrameSink` trait + subprocess sinks (mpv via pipe, ffmpeg remux to file). No GStreamer/libmpv dev libs on this machine yet. | – |
| `crates/ss-config` | profiles + settings TOML (same keys as the reference's `config.toml`), secrets via `keyring` | – |
| `crates/ss-cli` | `ss` binary wiring the above for manual testing against a real NAS | all |
| `crates/fake-nas` | Dev tool: offline stand-in for a NAS (`fake-nas` binary, axum). DSM Web API + `/ss_webstream_task/` MixStream on plain HTTP, synthetic world (6 cameras, 48 h of 30-min recordings, events, snapshots, alerts, licences, time-lapse), video = `~/.cache/synoptic/fixtures/test.h264` looped. Tests drive it with the real `ss-api`/`ss-stream` clients. | – (dev-deps: api, stream) |
| `crates/synoptic` | GTK4 app `synoptic` (app id `io.github.darious.Synoptic`) implementing the UI spec (private, see Gotchas). Module map in `docs/ui-plan.md`: `state.rs` (Store/Change bus, config persistence), `services.rs` (tokio + ss-api), `widgets/` shell, `pages/live` (grid with rows×cols/1+5/1+7 shapes + named layouts + DnD, slots, PTZ popover, multi-lane Cairo timeline with history playback), `video/` (GStreamer `appsrc → parse → decodebin3 → queue → [vapostproc] → videoconvert → gtk4paintablesink` + AAC audio branch, MixStream bridge), `driver.rs` (headless test driver, `SYNOPTIC_DRIVER=<fifo>`), `update_check.rs`, `pages/browse`, `pages/{licenses,settings,about}`, `dialogs/`. CSS: `data/style.css` tokens + `style-{live,browse,admin}.css`; icons `data/icons/ms-*-symbolic.svg` (Material Symbols, GResource). Needs `sudo apt install libgtk-4-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-gtk4 gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-vaapi gstreamer1.0-libav gstreamer1.0-tools fonts-ibm-plex` | api, stream, config |

## Commands

- Build: `cargo build --workspace`
- Run the app: `cargo run -p synoptic` (needs a Wayland/X11 session; `RUST_LOG=synoptic=debug` for logs)
- Test: `cargo test --workspace`
- Fake NAS for UI/CLI testing: `cargo run -p fake-nas -- [--port 5000] [--bind 127.0.0.1] [--fixtures DIR] [--otp CODE] [--user admin] [--password admin] [--seed 1]`; then `SS_PASSWORD=admin ss --host 127.0.0.1 --port 5000 --no-https -u admin cameras` or log the app in with HTTPS off
- Headless UI harness: `cargo build -p synoptic && UI_ID=0 tools/ui.sh start`, then `UI_ID=0 tools/ui.sh cmd "login dev 127.0.0.1 5000 admin admin" "page live"`, `UI_ID=0 tools/ui.sh shot <name>` (prints a ≤1024 px `.small.png` to view), `log`, `stop`. Driver command table: top of `crates/synoptic/src/driver.rs`. One `UI_ID` per concurrent harness.
- Install for the current user (release build, desktop entry, hicolor icons): `tools/install.sh` / `tools/install.sh --uninstall`
- CI: `.github/workflows/ci.yml` runs fmt/clippy/test in a `rust:1-trixie` container (GTK 4.18 is not on Ubuntu LTS runners), plus compile-only macOS (Homebrew) and Windows (MSYS2 UCRT64) jobs. Keep the tree portable: `keyring` backends are per-target in `ss-config/Cargo.toml`, Linux-only syscalls (`F_SETPIPE_SZ`) and the XDG launcher entry are `cfg`-gated; `ss-play`/`ss-cli` (mpv over a Unix socket) are excluded from the Windows job.
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Format: `cargo fmt --all` (check with `--check`)

## Conventions

- Edition 2024, `thiserror` for library errors, `anyhow` only in `ss-cli`.
- Async = tokio. HTTP = reqwest (rustls). WS = tokio-tungstenite.
- No network in tests: use `wiremock` for HTTP, byte fixtures for WS frames.
- Never log or put `_sid`, passwords or OTP codes in error messages (the
  reference project got bitten by this; see its `HttpStatusError` comment).
- Each crate is owned by one agent at a time; cross-crate API changes go
  through the workspace owner.

## Gotchas

- The UI spec (design README + JS-rendered HTML prototype) is private and lives outside the repo at `~/.cache/synoptic/design/handoff/`; the README is authoritative. `design/` is gitignored and must never be committed.
- This box is a headless tty: test the UI with `tools/ui.sh` (GTK Broadway + the driver in `crates/synoptic/src/driver.rs`), never by hand-waving. Agents must look at the `.small.png` screenshots before reporting UI work done.
- `tools/ui.sh` starts the app with `DBUS_SESSION_BUS_ADDRESS=disabled:`; without it a second `synoptic` becomes a GApplication remote of a running instance and exits 0 with an empty log. `shot` can report "nothing to render" while video is drawing — retry.
- Video: `decodebin3` picks `vah264dec`, whose NV12/VAMemory output does not negotiate with `gtk4paintablesink` when the sink has no GL context; keep `videoconvert` (and `vapostproc` when available) in front of the sink, and the software-decoder fallback in `video/pipeline.rs`.
- Recording/event playback (`dialogs/player.rs`) goes over MixStream (`stmSrc=2`, then `action=play … stmSrc=1`), the same path as Live View history and the one the Synology client uses; the HTTP `EventStream` URL is only used for time-lapse (`playbin3`). A real DSM answered `EventStream` with an empty body ("Stream doesn't contain enough data").
- App icon (a lens on a slate tile): `data/icons/io.github.darious.Synoptic{,-symbolic}.svg` (hand-written SVG, 128 px / 16 px grids), served from the gresource as `scalable/apps` + `symbolic/apps`, set with `window.set_icon_name(APP_ID)`, shown in the header, login dialog and About; `desktop.rs` writes the desktop entry + icons under `$XDG_DATA_HOME` at startup (Wayland shows a window icon only via a desktop entry; `tools/ui.sh` points `XDG_DATA_HOME` at the run dir); `data/io.github.darious.Synoptic.desktop` is the launcher. Check a change with the driver's `icon <name> <px> <png>` at 16/32/64/128.
- Icon names mirror `data/icons/ms-<name>-symbolic.svg`; filled variants are `<name>-fill` (e.g. `play_arrow-fill`), never `_fill`. A wrong name silently renders GTK's broken-image glyph, so check new icons on a screenshot.
- Live View focus: double-click (or the slot's "Fill view" button, driver `focus <i>`) shows one slot alone with one timeline lane; the other slots stay mounted and streaming, hidden, so the layout returns instantly. Escape or another double-click goes back.
- GTK4/GStreamer dev packages are installed; libmpv is not (ss-play uses the `mpv` binary).
- Thread-local `SERVICES` in `widgets/popovers.rs` and the raw `ApiRequest`s in
  `pages/browse/{recordings,timelapse}.rs` (GetThumbnail, TimeLapse) are known shortcuts.
- There is no NAS reachable from CI/agents. Integration against a NAS is
  manual via `ss-cli`.
- UI testing uses `fake-nas` (above) + GTK Broadway, not a real NAS. `fake-nas` needs `test.h264`/`test.aac`/`test.mp4` in the fixtures dir (makes `snap.jpg` with ffmpeg); its tests skip when they are missing.
- Vendor installer `*.exe`, the design zip and `~/.cache/synoptic/**` stay out of git. README screenshots (`docs/screenshots/`) come from the fake NAS via the harness, never from a real NAS.
- License is AGPL-3.0-or-later (`LICENSE`, workspace `license`, README, About); third-party credits stay in `NOTICE`.
- `ss-cli` wiremock tests spawn the `ss` binary via `CARGO_BIN_EXE_ss`, which cargo bakes in at compile time and does not re-track; after moving the checkout, `touch crates/ss-cli/tests/cameras_wiremock.rs`.
