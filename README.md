# rust_ss

Linux client for Synology Surveillance Station, to be written in Rust.

Status: reverse-engineering phase. See `docs/architecture.md` for the teardown
of the official Windows client (2.2.3-2598) and the port plan.

- `tools/nsis_extract.py` — stdlib-only NSIS installer extractor
- `tools/asar_extract.py` — stdlib-only Electron asar extractor
- Extracted vendor files and Ghidra snapshots live outside the repo in `~/.cache/rust_ss/`
