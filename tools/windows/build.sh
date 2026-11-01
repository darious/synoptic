#!/bin/bash
# Build and run Synoptic on Windows inside the MSYS2 UCRT64 shell
# (C:\msys64\ucrt64.exe). Installs the toolchain and the GStreamer plugin
# set the app needs at runtime, builds a release binary, and checks the
# decoder/sink elements are present.
#
#   bash tools/windows/build.sh          # install deps + build
#   bash tools/windows/build.sh run      # ... then start the app with logs
set -eu
export MSYSTEM=${MSYSTEM:-UCRT64}
P=mingw-w64-ucrt-x86_64
pacman -S --needed --noconfirm \
  $P-rust $P-pkgconf $P-gtk4 \
  $P-gstreamer $P-gst-plugins-base $P-gst-plugins-good $P-gst-plugins-bad \
  $P-gst-libav $P-gst-plugins-rs

export PATH=/ucrt64/bin:$PATH
cd "$(dirname "$0")/../.."
cargo build --release -p synoptic -p fake-nas

echo "--- GStreamer elements the app needs:"
for el in gtk4paintablesink h264parse h265parse decodebin3 avdec_h264 avdec_h265 d3d11h264dec d3d12h264dec autoaudiosink aacparse avdec_aac mulawdec; do
  if gst-inspect-1.0 "$el" >/dev/null 2>&1; then echo "  ok   $el"; else echo "  MISSING $el"; fi
done
echo "--- binary: $(pwd)/target/release/synoptic.exe (needs /ucrt64/bin on PATH, i.e. run from this shell)"

if [ "${1:-}" = "run" ]; then
  RUST_LOG=${RUST_LOG:-synoptic=debug,ss_stream=info} GST_DEBUG=${GST_DEBUG:-2} \
    ./target/release/synoptic.exe 2>&1 | tee synoptic-windows.log
fi
