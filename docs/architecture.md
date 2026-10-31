# Synology Surveillance Station Client 2.2.3-2598 — teardown notes

Source: `Synology Surveillance Station Client-2.2.3-2598_x64.exe` (NSIS installer,
sha256 `153a40be…3be`). Extracted with `tools/nsis_extract.py`; the Electron
app bundle with `tools/asar_extract.py`. Synology ships this client for Windows
and macOS only (their spec page lists "Windows 10 / macOS 11 or above").

## 1. What is in the box

| Component | Tech | Size | Role |
|---|---|---|---|
| `Synology Surveillance Station Client.exe` | **Electron 36.4.0 / Chrome 136**, patched to keep **PPAPI (Pepper)** plugin support | 204 MB | Shell. Hosts the NAS web UI in a `BrowserWindow`. |
| `resources/app.asar` → `main.js`, `preload.js` | JS (electron-webpack, source maps included) | 40 KB | Orchestration, bridge to web UI, download/export post-processing. |
| `NativeUI.exe` | **Qt 5** (Widgets, Network, WebSockets), OpenSSL 1.1, libcurl, UDT, live555 | 15 MB | Native login window, tray icon, settings store, QuickConnect / hole-punching, license activation. Talks to Electron over a localhost TCP socket. |
| `video_plugin.dll` | **PPAPI plugin** (exports `PPP_GetInterface`), same Qt/ffmpeg/live555 code base as NativeUI | 4 MB | Embedded as `application/x-video-plugin`. Receives stream URLs from the web UI, pulls RTSP/HTTP/WebSocket streams itself, decodes with ffmpeg (D3D11/MediaFoundation hw-accel), paints via `PPB_Graphics2D`, audio via `PPB_Audio`. |
| `ffmpeg.exe` (+ `avcodec-57`, `avformat-57`, `swscale-4`, …) | ffmpeg 3.x | 24 MB | Export post-processing (remux/transcode audio, burn overlay), HEVC MediaFoundation capability probe. |
| `dxc.exe`, `dxcompiler.dll`, `decodertest.exe`, `dpitest.exe` | DirectX | | GPU capability probes (Windows-only). |
| `udt.dll`, `pthreadVC2.dll` | | | UDT transport used by QuickConnect relay. |
| `h264_pattern.mp4`, `*_hint.jpg`, `ssc_*.ico`, `tray_icon.png` | assets | | |

The Electron build is **not stock**: strings such as
`content/browser/ppapi_plugin_process_host.cc` show Synology re-added Pepper,
which upstream Chromium removed in M109. Nothing from the Electron side is
reusable on Linux without rebuilding that fork.

## 2. Process topology

```
┌──────────────────────────┐   stdout + TCP 127.0.0.1:<port>   ┌──────────────────────┐
│ NativeUI.exe (Qt5)       │◄─────── JSON frames ─────────────►│ Electron main.js     │
│  • login dialog, OTP     │   delimiter: "`[W}:"              │  • BrowserWindow     │
│  • tray, settings        │                                    │  • preload bridge    │
│  • QuickConnect/UDT      │                                    │  • downloads/ffmpeg  │
└──────────────────────────┘                                    └──────────┬───────────┘
                                                                           │ loads
                                                                           ▼
                                             https://<nas>/webman/3rdparty/SurveillanceStation/
                                             (the ordinary SS web UI, served by the NAS)
                                                                           │ <embed type=application/x-video-plugin>
                                                                           ▼
                                                                ┌──────────────────────┐
                                                                │ video_plugin.dll     │
                                                                │ (PPAPI, per player)  │──► RTSP / HTTP / WS directly to NAS
                                                                └──────────────────────┘
```

Startup (from `main.js`):

1. Electron spawns `NativeUI.exe --electron synology`, reads its stdout.
2. NativeUI prints `{"MSG_TYPE":"LOCAL_SOCKETPORT","port":N}`; Electron connects
   to `localhost:N` and asks `{"MSG_TYPE":"CLIENT_SETTINGS","operation":"Get"}`.
3. Electron asks for the login window: `{"MSG_TYPE":"WINDOW","window_type":"Login"}`.
4. NativeUI performs the DSM login itself and replies with `LOGIN_INFO`
   (`url`, cookies `id`, `did`, `svs_dual_auth`, `blSelfsignedCert`,
   `certificate`, `proxyIp`, `proxyPort`).
5. Electron sets those cookies plus `SscLanguage`, opens
   `<url>/webman/3rdparty/SurveillanceStation/` and shows the window.
   Optional query params: `launchApp=<SS app class>`, `PlayLayoutId`,
   `MonitorFullscreen`.

### 2.1 NativeUI ⇄ Electron socket protocol

JSON objects separated by the literal token `` `[W}: ``.

| MSG_TYPE | Direction | Fields / meaning |
|---|---|---|
| `LOCAL_SOCKETPORT` | N→E (stdout) | `port` |
| `CLIENT_SETTINGS` | both | E→N `operation:"Get"`; N→E `data:{WinGeometry, WinStates, LoginLang, DownloadLocation, AlwaysAskDownloadLocation, EnableGpuDecoder, DisableGpuRender, string:{…localized strings…}}` or `debug:true` (opens devtools) |
| `LOGIN_INFO` | N→E | see above; E→N `cmd_login_info` for `--auto-login` style CLI logins |
| `WINDOW` | both | `window_type: Login \| Primary \| StandAlone`, `AppType` (0 VideoViewer, 3 Event, 6 Snapshot, 37 HelpBrowser, 42 SmartSearch) |
| `OPERATION` | N→E | `operation: Quit \| Logout \| Refresh` |
| `EVENT` | E→N | `event: LogoutDone \| Logout_Closed \| DeleteSystemTray \| WriteLog (Log)` |
| `JAVASCRIPT` | N→E | `script` — executed in the web UI |

### 2.2 preload bridge exposed to the NAS web UI

The web UI detects the desktop client through these globals (so the same web
app behaves differently in a browser vs. the client):

- `window.Client = { Version: 2598 }`
- `window.SynologySWC`: `GetSettings()`, `GetMaxGpuResource()`,
  `SendCommand(json)` (→ `OPERATION Quit/Fullscreen`, `JAVASCRIPT`),
  `GetClientType()` (7 = Windows, 8 = macOS, 1 = other), `IsWinFullscreen()`,
  `RegisterLogoutFunc`, `RegisterFullscreenChangeEvent`,
  `RegisterGpuCapReadyFunc`, `RegisterShowSystemTrayFunc`,
  `RegisterGetMonitorAppCntFunc`, `RegisterSaveDesktop`,
  `RegisterSaveOverlayImageFun`, `SetSignOutFlag()`,
  `LaunchStandaloneApp(type, param)`, `SetRestartHr(h)`, `SetCurLayoutId(id)`
- `window.PPapiPlugin.GetAdditionCookie()` → `";id = …;did = …"` string the
  web UI hands to the plugin so it can authenticate its own stream requests.
- `window.downloadMgr`: `saveOverlayImg`, `saveOverlayImgParam`,
  `startVideoProcess`, `OpenExplorer`.

### 2.3 Electron-side extras

- Chromium switches: `register-pepper-plugins=<video_plugin.dll>;application/x-video-plugin`,
  `enable-features=D3D11VideoDecoder`, `allow-command-line-plugins`,
  `max-active-webgl-contexts=256`; dark `nativeTheme`.
- Certificate check is a no-op (`setCertificateVerifyProc` always returns 0) —
  self-signed NAS certs are accepted after NativeUI validated them.
- Recording export: the web UI triggers a download; Electron intercepts it,
  and if the URL carries `overlayImgName` it asks the page for a PNG overlay
  and runs `ffmpeg -y -i <video> [-i overlay] -map … -c:v copy -c:a aac|copy`
  (G.726/G.726LE/AAC/MxPEG/HEVC special cases).
- GPU probe: `dxc.exe … 4` → max GPU resource = `921600 * value`;
  `ffmpeg -c:v hevc_mf -i h264_pattern.mp4 -an` to detect HEVC MediaFoundation.
- Standalone apps are just extra Electron processes launched with
  `--standalone <type> [--launchParam json]`; they reuse the same web UI.

## 3. Network protocols

### 3.1 DSM Web API (NativeUI and plugin)

APIs referenced by name: `SYNO.API.Auth` (login v6/logout v6),
`SYNO.API.Encryption` (getinfo v1 → RSA `public_key`, `cipherkey`,
`ciphertoken`, `server_time`; credentials are AES-CBC + RSA encrypted like the
DSM web login), `SYNO.Core.OTP.Mail`, `SYNO.Core.Desktop.Timeout`,
`SYNO.SDS.Session`, `SYNO.SDS.VirtualGroup`, `SYNO.Package`,
`SYNO.SurveillanceStation.{Info, System, CMS, DualAuth, License, Preload,
Recording(Keepalive v5)}`, `/webman/pingpong.cgi`.
Login extras seen: `enable_device_token`, `device_id`, `svs_dual_auth` cookie,
`X-SYNO-TOKEN` / `SynoToken` header. QuickConnect goes through
`https://global.quickconnect.to/Serv.php` with UDT relay or UDP hole punching
(`LibPunch`, `HolePunchingMgr`).

### 3.2 Media transport (video_plugin)

Module names recovered: `AccessHttp`, `AccessFile`, `AccessCache`,
`DemuxRtsp` (live555, RTSP over TCP/UDP/multicast and
`application/x-rtsp-tunnelled` over HTTP), `DemuxWs` (WebSocket stream),
`DemuxMF`, `DemuxMp4`, `DemuxAVI`, `DemuxMpegFile`, `DemuxMxpeg`,
`DemuxSpeaker` (talk-back audio), `DecoderVideoFFMpeg`, `DecoderAudioFFMpeg`,
`VideoOutQt`, `VideoOutD3d9`, `AudioOutMix`.
User agent: `Synology Media Player`. Session is passed as `Cookie: id=…`
(`x-sessioncookie` for RTSP-over-HTTP).
Playback URL fragments: `stmSrc=1&start=%1&flushTo=%1&reverse=false`,
`stmSrc=1&start=%1&reverse=true`; keep-alive via
`SYNO.SurveillanceStation.Recording&method=Keepalive&version=5`.

### 3.3 Web UI ⇄ plugin messages (PPB_Messaging)

Command vocabulary (strings): `PlayMediaURL`, `SetMediaURL`, `SendPlayerCmd`,
`seekMs`, `SetPlayerRate`, `pause`, `reverse`, `timeLapse`, `mute`, `volume`,
`GetSnapshot`, `SetFisheyeRegion`, `fisheyeRegionInfoUpdate`,
`fisheyePolygonUpdate`, `jsonOptions`, `jsonFisheye`, `keepAlive`,
`EnableGpuDecoder`, `MaxGpuDecoderNum`, `blUseGpu`, `dewarp`, `rtsp`, `cookie`,
`blHttps`, `width`, `height`.
Decompilation (§5) shows `SendPlayerCmd` carries a **query-string** payload
(`pause=true|false`, `speed=<float>`, `reverse=true|false`, `seekMs=<int>`,
`volume=<int>`, `mute=true|false`), not JSON.

## 4. What a Linux port actually needs

The client is **not** a native viewer. It is: a login helper + the NAS web UI in
Chromium + a native video decode/render plugin + export helpers. The web UI in
a plain Linux browser already works; what the client adds is hardware decoding
of many simultaneous streams, native login/tray/QuickConnect, and export
post-processing.

Two viable designs for Synoptic:

**A. Native Monitor Center (fully Rust, no web UI).**
Login via DSM Web API → camera list (`SYNO.SurveillanceStation.Camera`) →
live view via RTSP (`GetLiveViewPath`) with ffmpeg/GStreamer hw decode →
recordings via `SYNO.SurveillanceStation.Recording` + the `stmSrc` stream
URLs. Biggest effort, cleanest result, no dependency on Synology's web code.

**B. Web UI shell with a Rust video sidecar.**
WebView (wry/CEF) loads the NAS UI, injects the same `SynologySWC` /
`Client` / `PPapiPlugin` globals, and replaces `<embed type=application/x-video-plugin>`
with a custom element that forwards the same messages (§3.3) to a Rust
renderer. Rendering has to be composited into the page (e.g. decode in Rust,
push frames via WebCodecs/WebGL or a shared texture), because no Linux
WebView supports Pepper. Faster to first light, fragile against SS updates.

Either way the pieces that transfer directly: Web API/login flow (§3.1), stream
URL formats (§3.2), the ffmpeg export recipes (§2.3). See §7 for the decision.

## 5. Decompilation notes (video_plugin.dll, Ghidra 12.1.4 via `rea`)

Ghidra snapshot: `~/.cache/synoptic/ghidra_in/video_plugin.snapshot`
(19 973 procedures, 7 886 strings). Re-run with
`cd ~/.cache/synoptic/ghidra_in && rea decompile ./video_plugin.dll <addr> --snapshot ./video_plugin.snapshot`.

| Address | What it is | Finding |
|---|---|---|
| `FUN_180049ee0` | `DemuxBase::PlayMediaURL(QString url, ctx)` (`src\DemuxBase.cpp`) | Refuses if the demux thread is running (`0x3002`), otherwise copies the URL, parses it (`FUN_18001df90` splits scheme/host/path), starts the `DemuxThread`. Returns `0x3000` on success, `0x3007` on OOM. So the **web UI passes a complete stream URL**; the plugin does not build live URLs itself. |
| `FUN_18000fa30` | `MainVideoLoop::SendPlayerCmd(QString query)` (`MainVideoLoop.cpp`) | Parses with `QUrlQuery`. `pause=true` → player pause + 100 ms timer; `pause=false` → resume. `speed=<f>` → rate = `f` if ≥ 1 else `-1/f` (so 0.5 → −2, i.e. "1/2×"), logs `SetPlayerRate "Current rate %d, New rate %d"`. `reverse=true|false`, `seekMs=<ms>` (toInt → seek; triggers a WebSocket re-request when demux type == 3), `volume=<int>`, `mute=true|false`. |
| `FUN_1800e5e00` | history re-request builder | Emits `stmSrc=1&start=<sec>&flushTo=<sec>&reverse=false` or `stmSrc=1&start=<sec>&reverse=true` with `start = positionMs/1000`, then hands it to the stream object (vtable slot `0x38` = "send in-band command"). The string table next to it holds `&stamp=%1`, `ss_webstream_task`, `&dsId=`. |
| strings | WebSocket data path | `Data feeder: ContentLen=[%d] MediaType=[%d] Timestamp=[%lld]` — the same `mediaType`/`msec` frame header the browser client receives (see §6). |

Conclusion: the plugin's `DemuxWs` speaks **exactly the same
`/ss_webstream_task/?method=MixStream…` WebSocket protocol** as the browser
Monitor Center, with the same in-band `seekMs=…&stamp=n` /
`stmSrc=1&start=…` commands. Hardware decoding is the only thing it adds.

## 6. Prior art: renaudallard/surveillance-station-client

<https://github.com/renaudallard/surveillance-station-client> (BSD-2, Python
3.11 + GTK4 + libmpv, v0.15.0, 2026-10-21, ~26 k lines) is a complete native
Linux/BSD client built against the same NAS protocols, derived from sniffing
the browser Monitor Center. Clone kept at `~/.cache/synoptic/ref/ssc-gtk`. It
already has, and documents in code comments:

- **Login**: `SYNO.API.Auth` Login v6 with `otp_code`, `enable_device_token`,
  `device_id`, `device_name`; session via `_sid`; error-code tables.
- **Live view**: `SYNO.SurveillanceStation.Camera` `GetLiveViewPath`
  (`rtspPath`, `rtspOverHttpPath`, `mjpegHttpPath`, `multicstPath`) or the
  WebSocket `wss://<nas>/ss_webstream_task/?method=MixStream&stmSrc=0&blAudio=true&dsId=0&id=<cam>&devType=1&profile=<0|1|2>`.
  Frame = 4-byte big-endian header length + ASCII `k=v&…` header
  (`mediaType` 1 video / 2 audio, `msec`, `vdoCodec`, `adoCodec`, `close`) +
  payload (H.264/H.265 NAL without Annex-B start code).
- **History**: connect `…?method=MixStream&stmSrc=2&dsId=0&id=0`, then in-band
  `action=play&method=MixStream&blMux=true&browser=2&stmSrc=1&blAudio=true&mute=…&speed=1&reverse=false&recEvtType=…&mountId=…&archId=…&start=<s>&end=<s>&autoDrop=true&restart=true&pause=false&stamp=1&id=<recId>`;
  seeks inside the same file: `seekMs=<ms>&stamp=<n>`.
  Pause/speed/reverse were listed as "not yet captured" there — §5 above
  gives the native client's answer: `stmSrc=1&start=<s>&flushTo=<s>&reverse=false`
  / `stmSrc=1&start=<s>&reverse=true`, plus `&stamp=<n>`.
- Recordings/Events/Snapshots/TimeLapse/PTZ/HomeMode/AudioOut (push-to-talk)
  APIs, fisheye dewarp on GPU, AppImage packaging.

## 7. Recommendation for Synoptic

Design **A (native client)** is the right target, and the reference project
removes most of the protocol risk. A Rust equivalent would be:

| Layer | Crate candidates | Notes |
|---|---|---|
| HTTP/API | `reqwest` + `serde` | Port `api/auth.py`, `api/client.py`, `api/models.py` first; they are small. |
| WebSocket stream | `tokio-tungstenite` | Implement the MixStream framer (§6); feed Annex-B NALs into the decoder. |
| Decode/render | `gstreamer-rs` (appsrc → vaapi/nvdec → gtk4paintablesink) or `libmpv` via `libmpv-sys`/`mpv-client` | GStreamer gives a pure pipeline; libmpv is what the reference uses and handles RTSP for free. |
| UI | `gtk4-rs` + `libadwaita`, or `iced`/`egui` if GTK is unwanted | GTK4 makes a `gtk4paintablesink` grid trivial. |
| Config/secrets | `directories`, `toml`, `keyring` | Mirror the reference's `config.toml` layout to stay interoperable. |

Suggested order: API + login (incl. OTP) → camera list → single WebSocket
live slot with GStreamer → grid layouts → history playback with the §5/§6
in-band commands → recordings/events lists → export via `ffmpeg` recipes (§2.3).

Open items that still need a real NAS to confirm: the WS frame header field
list for H.265 and audio codecs, and whether `profile=` is honoured on
SS < 9.2.
