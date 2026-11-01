#!/bin/bash
# Headless UI harness: runs Synoptic on GTK's Broadway backend and feeds it
# driver commands (see crates/synoptic/src/driver.rs for the command list).
#
#   UI_ID=<n> tools/ui.sh start            start broadwayd :(7+n) and the app
#   UI_ID=<n> tools/ui.sh cmd "<line>"...  send driver lines, print their results
#   UI_ID=<n> tools/ui.sh shot <name>      screenshot → run/<n>/shots/<name>.png
#                                          and a ≤1024 px wide <name>.small.png
#   UI_ID=<n> tools/ui.sh log [lines]      tail the app log
#   UI_ID=<n> tools/ui.sh stop
#
# Each UI_ID gets its own Broadway display, FIFO, config dir and log, so
# several harnesses can run at once. Build first: cargo build -p synoptic.
set -u
ID=${UI_ID:-0}
DISP=":$((7 + ID))"
REPO=$(cd "$(dirname "$0")/.." && pwd)
D=$HOME/.cache/synoptic/run/$ID
FIFO=$D/cmd.fifo
mkdir -p "$D/shots" "$D/config"

kill_pidfile() {
  if [ -f "$1" ]; then
    kill "$(cat "$1")" 2>/dev/null
    rm -f "$1"
  fi
}

case "${1:-}" in
  start)
    kill_pidfile "$D/app.pid"
    kill_pidfile "$D/broadway.pid"
    sleep 0.3
    rm -f "$FIFO"
    mkfifo "$FIFO"
    gtk4-broadwayd "$DISP" >"$D/broadway.log" 2>&1 &
    echo $! >"$D/broadway.pid"
    sleep 0.5
    DBUS_SESSION_BUS_ADDRESS=disabled: XDG_CONFIG_HOME="$D/config" XDG_DATA_HOME="$D/data" GDK_BACKEND=broadway BROADWAY_DISPLAY="$DISP" \
      SYNOPTIC_DRIVER="$FIFO" \
      RUST_LOG="${RUST_LOG:-synoptic=debug,ss_api=info,ss_stream=info}" \
      "$REPO/target/debug/synoptic" >"$D/app.log" 2>&1 &
    echo $! >"$D/app.pid"
    for _ in $(seq 1 60); do
      grep -q 'driver ready' "$D/app.log" 2>/dev/null && break
      sleep 0.25
    done
    tail -n 3 "$D/app.log"
    ;;
  cmd)
    shift
    n=$(wc -l <"$D/app.log")
    printf '%s\n' "$@" >"$FIFO"
    for _ in $(seq 1 300); do
      if [ "$(tail -n +$((n + 1)) "$D/app.log" | grep -c -E '^(ok|err) ')" -ge "$#" ]; then break; fi
      sleep 0.1
    done
    tail -n +$((n + 1)) "$D/app.log" | grep -E '^(ok|err|  )'
    ;;
  shot)
    name=${2:?name}
    "$0" cmd "shot $D/shots/$name.png" >/dev/null
    for f in "$D/shots/$name"*.png; do
      case "$f" in *.small.png) continue ;; esac
      ffmpeg -y -loglevel error -i "$f" -vf "scale='min(1024,iw)':-1" "${f%.png}.small.png"
      echo "${f%.png}.small.png"
    done
    ;;
  log)
    tail -n "${2:-40}" "$D/app.log"
    ;;
  stop)
    kill_pidfile "$D/app.pid"
    kill_pidfile "$D/broadway.pid"
    echo stopped
    ;;
  *)
    sed -n '2,13p' "$0"
    ;;
esac
