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
| `crates/synoptic` | GTK4 app `synoptic` (app id `io.github.darious.Synoptic`) implementing the UI spec (private, see Gotchas). Module map in `docs/ui-plan.md`: `state.rs` (Store/Change bus, config persistence), `services.rs` (tokio + ss-api), `widgets/` shell, `pages/live` (grid, slots, Cairo timeline), `video/` (GStreamer `appsrc → parse → decodebin3 → gtk4paintablesink`, MixStream bridge), `pages/browse`, `pages/{licenses,settings,about}`, `dialogs/`. CSS: `data/style.css` tokens + `style-{live,browse,admin}.css`; icons `data/icons/ms-*-symbolic.svg` (Material Symbols, GResource). Needs `sudo apt install libgtk-4-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-gtk4 gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-vaapi gstreamer1.0-libav gstreamer1.0-tools fonts-ibm-plex` | api, stream, config |

## Commands

- Build: `cargo build --workspace`
- Run the app: `cargo run -p synoptic` (needs a Wayland/X11 session; `RUST_LOG=synoptic=debug` for logs)
- Test: `cargo test --workspace`
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
- This box is a headless tty (no DISPLAY/Wayland, no Xvfb); the UI cannot be screenshotted here.

- GTK4/GStreamer dev packages are installed; libmpv is not (ss-play uses the `mpv` binary).
- Thread-local `SERVICES` in `widgets/popovers.rs` and the raw `ApiRequest`s in
  `pages/browse/{recordings,timelapse}.rs` (GetThumbnail, TimeLapse) are known shortcuts.
- There is no NAS reachable from CI/agents. Integration against a NAS is
  manual via `ss-cli`.
- Vendor installer `*.exe`, the design zip and `~/.cache/synoptic/**` stay out of git.
- `ss-cli` wiremock tests spawn the `ss` binary via `CARGO_BIN_EXE_ss`, which cargo bakes in at compile time and does not re-track; after moving the checkout, `touch crates/ss-cli/tests/cameras_wiremock.rs`.
