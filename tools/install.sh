#!/bin/bash
# User-level install: release binary → ~/.local/bin, desktop entry and
# icons → ~/.local/share. Run from the repo: tools/install.sh [--uninstall]
set -eu
REPO=$(cd "$(dirname "$0")/.." && pwd)
APP=io.github.darious.Synoptic
BIN=${XDG_BIN_HOME:-$HOME/.local/bin}
SHARE=${XDG_DATA_HOME:-$HOME/.local/share}
ICONS=$SHARE/icons/hicolor

if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$BIN/synoptic" "$SHARE/applications/$APP.desktop" \
    "$ICONS/scalable/apps/$APP.svg" "$ICONS/symbolic/apps/$APP-symbolic.svg"
  echo "removed"
else
  (cd "$REPO" && cargo build --release -p synoptic)
  install -Dm755 "$REPO/target/release/synoptic" "$BIN/synoptic"
  install -Dm644 "$REPO/crates/synoptic/data/$APP.desktop" "$SHARE/applications/$APP.desktop"
  install -Dm644 "$REPO/crates/synoptic/data/icons/$APP.svg" "$ICONS/scalable/apps/$APP.svg"
  install -Dm644 "$REPO/crates/synoptic/data/icons/$APP-symbolic.svg" "$ICONS/symbolic/apps/$APP-symbolic.svg"
  echo "installed synoptic to $BIN (make sure it is on your PATH)"
fi
command -v update-desktop-database >/dev/null && update-desktop-database "$SHARE/applications" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$ICONS" || true
