# rust_ss

Linux client for Synology Surveillance Station, to be written in Rust.

Status: first complete build, not yet run against a NAS. `cargo run -p ss-ui`
starts the GTK4 app; `cargo run -p ss-cli -- --help` is the headless test tool.
See `docs/architecture.md` for the teardown of the official Windows client
(2.2.3-2598), `docs/ui-plan.md` for the UI module map, and `design/handoff/`
for the UI spec.

- `tools/nsis_extract.py` — stdlib-only NSIS installer extractor
- `tools/asar_extract.py` — stdlib-only Electron asar extractor
- Extracted vendor files and Ghidra snapshots live outside the repo in `~/.cache/rust_ss/`
