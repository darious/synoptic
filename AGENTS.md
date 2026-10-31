# rust_ss — agent map

Linux client for Synology Surveillance Station in Rust. Reverse-engineering
notes and the port plan: `docs/architecture.md` (read §5–§7 first).
Reference implementation (Python/GTK, BSD-2) is cloned at
`~/.cache/rust_ss/ref/ssc-gtk`; port its *protocol knowledge*, not its UI.

## Layout

| Crate | Scope | Depends on |
|---|---|---|
| `crates/ss-api` | DSM Web API: `SYNO.API.Info` discovery, `SYNO.API.Auth` login v6 (OTP, device token), error tables, Camera/Recording/Event/Snapshot/PTZ calls, typed models | – |
| `crates/ss-stream` | `/ss_webstream_task/` MixStream WebSocket: URL builders (live `stmSrc=0`, history `stmSrc=2`), frame parser (4-byte BE header length + `k=v&…` header + payload), in-band commands (`action=play…`, `seekMs=…&stamp=n`, `stmSrc=1&start=…`), keepalive, reconnect | – (takes a session id/cookie, not `ss-api` types) |
| `crates/ss-play` | `FrameSink` trait + subprocess sinks (mpv via pipe, ffmpeg remux to file). No GStreamer/libmpv dev libs on this machine yet. | – |
| `crates/ss-config` | profiles + settings TOML (same keys as the reference's `config.toml`), secrets via `keyring` | – |
| `crates/ss-cli` | `ss` binary wiring the above for manual testing against a real NAS | all |
| UI crate | **not started** — toolkit still being chosen | |

## Commands

- Build: `cargo build --workspace`
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

- No GTK4/GStreamer/libmpv dev packages installed; `mpv` and `ffmpeg`
  binaries exist. Anything needing dev libs waits for the UI decision.
- There is no NAS reachable from CI/agents. Integration against a NAS is
  manual via `ss-cli`.
- Vendor installer `*.exe` and `~/.cache/rust_ss/**` stay out of git.
