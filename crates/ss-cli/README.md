# ss-cli

`ss` is the headless test client for the `ss-*` crates: it logs in with
`ss-api`, saves profiles and credentials with `ss-config`, opens MixStream
WebSockets with `ss-stream` and shows or records the video with `ss-play`
(mpv / ffmpeg subprocesses). It is the only way to exercise the stack
against a real Surveillance Station; CI has no NAS.

```
cargo run -p ss-cli -- --help
```

## Commands

| command | what |
|---|---|
| `ss login --host H [--port P] [--https/--no-https] [--insecure] --user U [--profile NAME] [--otp CODE] [--trust-device] [--password-stdin] [--set-default] [--no-keyring]` | log in, print SS version / model / camera count, save the profile, store the credentials in the keyring |
| `ss cameras` | table: id, name, model, vendor, status, codec, ptz, audio, live-profile |
| `ss paths <camera-id> [--quality high\|balanced\|low]` | the `GetLiveViewPath` fields and the MixStream WebSocket URL |
| `ss live <camera-id> [--quality …] [--sink mpv\|raw\|mp4] [--out FILE] [--seconds N] [--codec h264\|h265]` | play / record the live MixStream |
| `ss history <camera-id> --at "YYYY-MM-DD HH:MM:SS" [--speed F] [--reverse] [--window SECS] [--sink …] [--out …] [--seconds N]` | find the recording covering `--at` and play it from there |
| `ss recordings [--camera ID] [--from T] [--to T] [--limit 20] [--offset 0]` | list recordings |
| `ss download <recording-id> --out FILE` | download a recording |
| `ss rtsp <camera-id> [--protocol rtsp\|rtsp-over-http\|multicast\|mjpeg] [--seconds N]` | play the camera's RTSP URL in mpv |

Global flags (before or after the subcommand): `--profile NAME`, `--host`,
`--port`, `--https` / `--no-https`, `--insecure`, `-u/--user`, `-v`/`-vv`.
Every command but `login` loads the profile (`--profile`, else
`general.default_profile` in `~/.config/surveillance-station/config.toml`),
overlays the connection flags, takes the password from `$SS_PASSWORD`, else
the keyring, else a prompt, and logs in with automatic re-login on session
expiry. `--quality` is the Surveillance Station stream profile; the word
"profile" on the command line always means a connection profile.

Secrets: the sid, password, OTP and device token are never printed;
tracing output is additionally scrubbed (`output::redact`). `-v` raises
only the `ss_*` crates to debug (`-vv` trace); `RUST_LOG` replaces the
filter entirely. Exit codes: 0 ok, 1 error, 2 usage, 3 two-factor code
required.

Times are local wall-clock (`YYYY-MM-DD HH:MM[:SS]`, `T` separator
accepted, or unix seconds). Headless machine: use `--sink raw` or
`--sink mp4`; mpv also honours `SURVEILLANCE_MPV_OPTS` / `SS_PLAY_MPV_OPTS`
(e.g. `vo=null` for a decode-only check) and `SURVEILLANCE_AO`.

## Tests (no NAS)

`cargo test -p ss-cli`: unit tests for argument parsing, time parsing,
the recording-selection maths and redaction; `tests/cameras_wiremock.rs`
runs the built `ss` binary against a wiremock NAS (API info, login,
camera list, OTP and bad-password exits, `login` saving a profile);
`tests/pump_h264.rs` generates an H.264 stream with ffmpeg and pushes it
through the same frame→sink pump the `live` command uses, from a fake
event source into `RawFileSink`, then checks it with ffprobe (skipped
with a message when ffmpeg / libx264 is missing).

## Manual test plan against a NAS

Run these in order; each step depends on the previous one working.
Use `-v` on any step that fails. `H`, `U`, `ID` are your NAS host, DSM
account and a camera id from step 2.

1. **Login** — `ss login --host H --user U --insecure` (type the
   password; or `SS_PASSWORD=… ss login …`, or `echo "$PW" | ss login
   --password-stdin …`).
   Success: `Logged in to https://H:5001 as U`, a line
   `Surveillance Station 9.x.y-build on <model> (<hostname>), N camera(s),
   M licence(s)`, `Saved profile "H" (default) to …/config.toml`, and
   `Credentials stored in the system keyring.` (or a warning that the
   keyring is unavailable — then export `SS_PASSWORD` for the rest).
   With 2FA: the first attempt exits 3 with `two-factor authentication
   required … pass --otp CODE`; repeat with `--otp 123456 --trust-device`
   and expect `Trusted-device token: received`. A later plain `ss cameras`
   must then log in without an OTP (the token is in the profile).
   Wrong password: exit 1, `No such account or incorrect password (code 400)`.
2. **Cameras** — `ss cameras`.
   Success: one row per camera matching DSM's camera list (status
   `enabled` for running cameras, `disabled`/`disconnected` otherwise),
   codec `H.264`/`H.265`, `ptz yes` on PTZ models, `live-profile`
   high/balanced/low from each camera's Live View setting, `N camera(s)`.
3. **Paths** — `ss paths ID`.
   Success: `rtspPath rtsp://…`, `rtspOverHttpPath http://…`, an
   `mjpegHttpPath` (or `-`), `multicstPath` (usually `-`), and
   `websocket(balanced) wss://H:5001/ss_webstream_task/?method=MixStream&stmSrc=0&…&id=ID&…&profile=1`.
   No `_sid` anywhere in the output.
4. **Live, recorded to a file (no window needed)** —
   `ss live ID --sink raw --seconds 10`.
   Success: `[live] writing h264 to ss-camID-<unix>.h264`, a stats line
   every 2 s with `frames=` growing at the camera's frame rate,
   `reconnects=0`, then `N video frame(s), … stopped: Requested`.
   `ffprobe ss-camID-*.h264` reports the codec; `mpv ss-camID-*.h264`
   plays 10 s. Repeat with `--sink mp4 --out live.mp4 --seconds 10` and
   `ffprobe live.mp4` shows one video stream of ~10 s. Try
   `--quality low` and `--quality high` and see the bytes/2 s change.
5. **Live in a window** — `ss live ID` (desktop session).
   Success: an mpv window titled `ss camID` shows the camera within a
   second or two, stays smooth, Ctrl-C in the terminal prints
   `interrupted`, closes the window and the summary line. Closing the mpv
   window instead ends the command with `sink refused a frame: mpv exited
   …`, which is expected. Leave it running for > 2 min to see that
   `reconnects=` stays 0 for live (DSM keeps live sockets open) and that
   the keepalive works.
6. **Recordings** — `ss recordings --camera ID --limit 10`, then with
   `--from "YYYY-MM-DD 00:00" --to "YYYY-MM-DD 23:59"`.
   Success: ids, `start`/`stop` in local time matching DSM's Recording
   list, `length`, `size`, `type`, detection `labels`, and
   `n of total recording(s)`. A recording still being written shows
   `(recording)` as its stop.
7. **History** — pick a start time inside a listed recording and run
   `ss history ID --at "YYYY-MM-DD HH:MM:SS" --sink raw --seconds 15`.
   Success: `[history] recording <id> (start .. stop, length), start=<s>s
   end=<len>s speed=1 reverse=false`, stats lines with `last_msec=`
   increasing from about `start*1000`, and a playable `.h264`. Then on a
   desktop: `ss history ID --at … --speed 4` (frames arrive ~4× faster,
   `last_msec` advances 4 s per second), `--reverse` (`last_msec`
   decreases), and a time with no recording (expect `nothing covers …;
   nearest recording is … away`, playing the nearest one from its edge).
   Let a history session run past 2 min to see DSM drop it and
   `reconnects=` go to 1 while `last_msec` continues from where it was.
8. **Download** — `ss download <recording-id> --out rec.mp4` with an id
   from step 6.
   Success: `N MiB written to rec.mp4` with N close to the listed size;
   `ffprobe rec.mp4` shows video (and audio if the camera has it).
9. **RTSP** — `ss rtsp ID --seconds 15`, then `--protocol rtsp-over-http`.
   Success: mpv plays the camera over TCP RTSP and quits after 15 s with
   `mpv stopped`; a multicast camera also works with
   `--protocol multicast`. Failure to connect shows mpv's error via
   `mpv exited with …`.
10. **Session expiry** — in DSM, kill the `ss` session (Control Panel →
    ... → Security / Connected users) while a long `ss live … --sink raw`
    is running, or run a command after the DSM session timeout.
    Success: the API call that hits the dead session is retried after an
    automatic re-login (visible with `-v`: `session error, re-logging in`)
    and the command completes.

Keep the `.h264` of any stream that misbehaves: it is byte-for-byte what
the NAS sent (plus start codes) and is what a bug report needs.
