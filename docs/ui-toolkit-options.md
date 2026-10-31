# UI toolkit options (input for the design work)

The non-UI crates (`ss-api`, `ss-stream`, `ss-play`, `ss-config`) are toolkit
agnostic. The toolkit decision only affects how decoded video gets on screen.

| Option | Video path | Look | System deps to install (Debian 13) | Notes |
|---|---|---|---|---|
| **gtk4-rs + libadwaita** | `gtk4paintablesink` from GStreamer, or libmpv render API into a `GLArea` | native GNOME; themable with CSS | `libgtk-4-dev libadwaita-1-dev libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-plugins-{good,bad} gstreamer1.0-vaapi gstreamer1.0-gtk4` | Most mature video story on Linux. Wayland + VAAPI zero-copy works. |
| **iced** | custom `wgpu` texture updated from decoded frames (GStreamer `appsink` or ffmpeg via `ffmpeg-next`) | fully custom, Elm-style | `libgstreamer1.0-dev …` or `libavcodec-dev libavformat-dev libavutil-dev` | `iced_video_player` crate exists (GStreamer based). Easy to make it look like whatever the design says. |
| **Slint** | `slint::Image` from frames; GStreamer example in Slint repo | fully custom, declarative `.slint` files | same as iced | Good designer/developer split: the `.slint` files *are* the design deliverable. |
| **egui/eframe** | texture upload per frame | immediate-mode, utilitarian | same as iced | Fastest to prototype, hardest to make pretty. |
| **Tauri / wry** | WebCodecs in the WebView, or MSE | HTML/CSS | `libwebkit2gtk-4.1-dev` | WebKitGTK has no HEVC and weak hardware decode; rejected for the same reason the Synology web UI is. |

Decode engine, independent of toolkit:

- **GStreamer**: `appsrc ! h264parse/h265parse ! vah264dec/vah265dec (or decodebin3) ! sink`. Handles VAAPI/NVDEC selection, many streams, and gives a `Paintable` for GTK. Rust bindings are excellent.
- **libmpv**: what the reference app uses. Simplest for RTSP URLs (mpv does the networking), render API gives an OpenGL FBO. Needs a GL surface in the toolkit.
- **ffmpeg-next**: lowest level, most control, most work.

Recommendation to carry into the design: GTK4 + libadwaita + GStreamer if the
design is "a good GNOME app"; Slint + GStreamer if the design is bespoke.
