//! One function per subcommand, and [`dispatch`].

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use ss_api::{Camera, LiveProtocol, RecordingFilter, StreamProfile as ApiProfile};
use ss_config::{Profile, SecretError};
use ss_play::MpvUrlPlayer;
use ss_stream::{MixStreamClient, MixStreamConfig, Speed, StreamProfile};

use crate::cli::{
    Cli, Command, DownloadArgs, HistoryArgs, LiveArgs, LoginArgs, PathsArgs, PlayArgs,
    RecordingsArgs, RtspArgs,
};
use crate::output::{Table, format_bytes, redact, video_codec_name, yes_no};
use crate::pump::{self, PumpOptions};
use crate::select::{Selection, play_params, select_recording, window_around};
use crate::session::{self, Connected, ENV_PASSWORD};
use crate::sink::SinkSpec;
use crate::timefmt::{format_duration, format_local, parse_local};

/// Run the subcommand: `login` stands alone; everything else logs in,
/// runs, and logs out again (even on failure).
pub async fn dispatch(cli: Cli) -> Result<()> {
    if let Command::Login(args) = &cli.command {
        return login(&cli, args).await;
    }
    preflight(&cli.command)?;
    let conn = session::connect(&cli).await?;
    let result = match &cli.command {
        Command::Login(_) => unreachable!("handled above"),
        Command::Cameras => cameras(&conn).await,
        Command::Paths(args) => paths(&conn, args).await,
        Command::Live(args) => live(&conn, args).await,
        Command::History(args) => history(&conn, args).await,
        Command::Recordings(args) => recordings(&conn, args).await,
        Command::Download(args) => download(&conn, args).await,
        Command::Rtsp(args) => rtsp(&conn, args).await,
    };
    conn.logout().await;
    result
}

/// Reject bad times and speeds before spending a login on them.
fn preflight(command: &Command) -> Result<()> {
    match command {
        Command::History(args) => {
            parse_local(&args.at).context("--at")?;
            Speed::new(args.speed).ok_or_else(|| anyhow!("--speed must be > 0"))?;
        }
        Command::Recordings(args) => {
            if let Some(from) = &args.from {
                parse_local(from).context("--from")?;
            }
            if let Some(to) = &args.to {
                parse_local(to).context("--to")?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Resolves on Ctrl-C or after `seconds`.
async fn stop_signal(seconds: Option<f64>) {
    let timer = async {
        match seconds {
            Some(s) if s.is_finite() && s >= 0.0 => {
                tokio::time::sleep(Duration::from_secs_f64(s)).await;
            }
            _ => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => eprintln!("\ninterrupted"),
        () = timer => eprintln!("time is up"),
    }
}

// ----- login ----------------------------------------------------------------

async fn login(cli: &Cli, args: &LoginArgs) -> Result<()> {
    let loaded = ss_config::load().context("loading the configuration")?;
    let mut config = loaded.config;
    let target = session::login_target(cli, &config)?;
    let profile_name = target.profile.clone().expect("login target has a profile");
    let creds = session::obtain_credentials(&target, args.password_stdin, false).await?;
    let client = session::client_for(&target).await?;
    let session = session::login(
        &client,
        &creds,
        args.otp.as_deref(),
        args.trust_device,
        false,
    )
    .await?;

    let info = client.ss_info().await.context("Info.GetInfo")?;
    let version = info
        .version
        .as_ref()
        .map_or_else(|| "unknown".to_owned(), ToString::to_string);
    println!("Logged in to {} as {}", target.base_url(), creds.username);
    println!(
        "Surveillance Station {version} on {} ({}), {} camera(s), {} licence(s)",
        if info.ds_model_name.is_empty() {
            "unknown model"
        } else {
            &info.ds_model_name
        },
        if info.hostname.is_empty() {
            &target.host
        } else {
            &info.hostname
        },
        info.camera_number,
        info.license_number
    );
    if args.trust_device {
        println!(
            "Trusted-device token: {}",
            if session.did.is_some() {
                "received (later logins skip the OTP)"
            } else {
                "not issued by the NAS"
            }
        );
    }

    // Save the profile.
    let mut profile = config
        .profiles
        .get(&profile_name)
        .cloned()
        .unwrap_or_else(|| Profile::new(&target.host));
    profile.host.clone_from(&target.host);
    profile.port = target.port;
    profile.https = target.https;
    profile.verify_ssl = !target.accept_invalid_certs;
    profile.username = Some(creds.username.clone());
    if let Some(did) = session.did.clone().or(target.device_id.clone()) {
        profile.device_id = did;
    }
    config.add_profile(&profile_name, profile);
    if args.set_default {
        config.general.default_profile.clone_from(&profile_name);
    }
    ss_config::save(&config).context("saving the configuration")?;
    let file = ss_config::Paths::from_env()
        .map(|p| p.config_file().display().to_string())
        .unwrap_or_default();
    println!(
        "Saved profile {profile_name:?}{} to {file}",
        if config.general.default_profile == profile_name {
            " (default)"
        } else {
            ""
        }
    );

    // Save the credentials.
    if args.no_keyring {
        println!("Credentials not stored (--no-keyring); set {ENV_PASSWORD} for later commands.");
    } else {
        match session::keyring_store(&profile_name, &creds).await {
            Ok(()) => println!("Credentials stored in the system keyring."),
            Err(SecretError::Unavailable(why)) => {
                eprintln!(
                    "warning: system keyring unavailable ({why}); \
                     set {ENV_PASSWORD} for later commands"
                );
            }
            Err(e) => eprintln!("warning: could not store credentials: {e}"),
        }
    }
    client.logout().await;
    Ok(())
}

// ----- cameras ---------------------------------------------------------------

/// Camera list plus the `videoCodec` number `ss-api`'s model leaves out,
/// read from the same `Camera.List` answer.
async fn list_cameras_with_codec(conn: &Connected) -> Result<Vec<(Camera, i64)>> {
    let data = conn
        .client
        .request(
            "SYNO.SurveillanceStation.Camera",
            "List",
            9,
            &[("basic", "true"), ("streamInfo", "true"), ("ptz", "true")],
        )
        .await
        .context("Camera.List")?;
    let raw = data
        .get("cameras")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::with_capacity(raw.len());
    for item in raw {
        let codec = item
            .get("videoCodec")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0);
        let cam: Camera = serde_json::from_value(item).context("decoding Camera.List entry")?;
        out.push((cam, codec));
    }
    Ok(out)
}

async fn cameras(conn: &Connected) -> Result<()> {
    let cams = list_cameras_with_codec(conn).await?;
    let ids: Vec<i64> = cams.iter().map(|(c, _)| c.id).collect();
    // Live View profiles come from GetInfo only; a failure just blanks the column.
    let profiles: std::collections::HashMap<i64, ApiProfile> = if ids.is_empty() {
        Default::default()
    } else {
        match conn.client.camera_info(&ids).await {
            Ok(infos) => infos
                .into_iter()
                .filter_map(|c| c.def_live_profile.map(|p| (c.id, p)))
                .collect(),
            Err(e) => {
                tracing::warn!("Camera.GetInfo failed ({e}); live profiles unknown");
                Default::default()
            }
        }
    };
    let mut table = Table::new([
        "id",
        "name",
        "model",
        "vendor",
        "status",
        "codec",
        "ptz",
        "audio",
        "live-profile",
    ]);
    for (cam, codec) in &cams {
        table.row([
            cam.id.to_string(),
            cam.name.clone(),
            dash(&cam.model),
            dash(&cam.vendor),
            status_name(cam.status),
            video_codec_name(*codec),
            yes_no(cam.is_ptz).to_owned(),
            yes_no(cam.has_audio).to_owned(),
            profiles
                .get(&cam.id)
                .map_or_else(|| "-".to_owned(), |p| profile_name(*p).to_owned()),
        ]);
    }
    print!("{}", table.render());
    println!("{} camera(s)", table.len());
    Ok(())
}

fn dash(s: &str) -> String {
    if s.is_empty() { "-".into() } else { s.into() }
}

fn status_name(s: ss_api::CameraStatus) -> String {
    use ss_api::CameraStatus::*;
    match s {
        Enabled => "enabled".into(),
        Disconnected => "disconnected".into(),
        Disabled => "disabled".into(),
        Other(n) => format!("status#{n}"),
    }
}

fn profile_name(p: ApiProfile) -> &'static str {
    match p {
        ApiProfile::High => "high",
        ApiProfile::Balanced => "balanced",
        ApiProfile::Low => "low",
    }
}

/// `--quality`, else the camera's Live View setting, else Balanced.
async fn resolve_quality(
    conn: &Connected,
    camera_id: i64,
    flag: Option<crate::cli::Quality>,
) -> StreamProfile {
    if let Some(q) = flag {
        return q.into();
    }
    match conn.client.live_view_profile(camera_id).await {
        Ok(p) => StreamProfile::from_number(p.number()).unwrap_or_default(),
        Err(e) => {
            tracing::warn!("Camera.GetInfo failed ({e}); using the balanced profile");
            StreamProfile::default()
        }
    }
}

fn camera_u32(id: i64) -> Result<u32> {
    u32::try_from(id).map_err(|_| anyhow!("camera id {id} is out of range"))
}

// ----- paths ------------------------------------------------------------------

async fn paths(conn: &Connected, args: &PathsArgs) -> Result<()> {
    let paths = conn
        .client
        .live_view_paths(&[args.camera_id])
        .await
        .context("Camera.GetLiveViewPath")?;
    let Some(p) = paths.first() else {
        bail!("camera {} has no live view path", args.camera_id);
    };
    println!("camera {}", p.id.unwrap_or(args.camera_id));
    for proto in [
        LiveProtocol::Rtsp,
        LiveProtocol::RtspOverHttp,
        LiveProtocol::Mjpeg,
        LiveProtocol::Multicast,
    ] {
        println!(
            "  {:<18} {}",
            proto.field(),
            p.path(proto).map_or_else(|| "-".to_owned(), redact)
        );
    }
    let quality = resolve_quality(conn, args.camera_id, args.quality).await;
    let url = ss_stream::live_url(conn.client.base_url(), camera_u32(args.camera_id)?, quality)?;
    println!(
        "  {:<18} {url}",
        format!("websocket({})", quality_name(quality))
    );
    println!("  (authentication: Cookie: id=<sid> on the upgrade; not in the URL)");
    Ok(())
}

fn quality_name(q: StreamProfile) -> &'static str {
    match q {
        StreamProfile::High => "high",
        StreamProfile::Balanced => "balanced",
        StreamProfile::Low => "low",
    }
}

// ----- live / history ---------------------------------------------------------

fn pump_options(play: &PlayArgs, label: &str, stop_at_ms: Option<u64>) -> PumpOptions {
    PumpOptions {
        codec_override: play.codec.map(Into::into),
        stats_interval: Some(Duration::from_secs(2)),
        stop_at_ms,
        label: label.to_owned(),
    }
}

async fn live(conn: &Connected, args: &LiveArgs) -> Result<()> {
    let quality = resolve_quality(conn, args.camera_id, args.quality).await;
    let url = ss_stream::live_url(conn.client.base_url(), camera_u32(args.camera_id)?, quality)?;
    eprintln!(
        "[live] camera {} quality={} sink={:?}",
        args.camera_id,
        quality_name(quality),
        args.play.sink
    );
    let config = MixStreamConfig::live(url, conn.sid()?);
    let (handle, mut events) = MixStreamClient::connect(config, conn.ws_connector());
    let spec = SinkSpec::from_args(&args.play, &format!("cam{}", args.camera_id), 1.0);
    let opts = pump_options(&args.play, "live", None);
    let result = pump::run(&mut events, &spec, &opts, stop_signal(args.play.seconds)).await;
    handle.stop().await;
    report(result.map(|s| (s, None)))
}

async fn history(conn: &Connected, args: &HistoryArgs) -> Result<()> {
    let at = parse_local(&args.at).context("--at")?;
    let speed = Speed::new(args.speed).ok_or_else(|| anyhow!("--speed must be > 0"))?;
    let window = window_around(at, args.window);
    eprintln!(
        "[history] camera {} at {} ({at}); searching {} .. {}",
        args.camera_id,
        format_local(at),
        format_local(window.from),
        format_local(window.to)
    );
    let mut recs = Vec::new();
    let mut offset = 0u32;
    loop {
        let filter = RecordingFilter::camera(args.camera_id)
            .between(window.from, window.to)
            .page(offset, 100);
        let page = conn
            .client
            .list_recordings(&filter)
            .await
            .context("Recording.List")?;
        let n = page.items.len();
        recs.extend(page.items);
        offset += n as u32;
        if n == 0 || i64::from(offset) >= page.total || offset >= 1000 {
            break;
        }
    }
    let selection = select_recording(&recs, at).ok_or_else(|| {
        anyhow!(
            "no recordings for camera {} between {} and {}; widen --window",
            args.camera_id,
            format_local(window.from),
            format_local(window.to)
        )
    })?;
    let rec = selection.recording();
    match &selection {
        Selection::Covering(_) => {}
        Selection::Nearest { gap_secs, .. } => eprintln!(
            "[history] nothing covers {}; nearest recording is {} away",
            format_local(at),
            format_duration(*gap_secs)
        ),
    }
    let params = play_params(rec, at, speed, args.reverse);
    eprintln!(
        "[history] recording {} ({} .. {}, {}), start={}s end={}s speed={} reverse={}",
        rec.id,
        format_local(rec.start_time),
        format_local(rec.stop_time),
        format_duration(rec.duration_secs()),
        params.start_s,
        params.end_s,
        params.speed,
        params.reverse
    );
    let url = ss_stream::history_url(conn.client.base_url())?;
    let config = MixStreamConfig::history(url, conn.sid()?, params.clone());
    let (handle, mut events) = MixStreamClient::connect(config, conn.ws_connector());
    let spec = SinkSpec::from_args(
        &args.play,
        &format!("cam{}-rec{}", args.camera_id, rec.id),
        args.speed,
    );
    let stop_at = (!args.reverse && params.end_s > 0).then(|| params.end_s * 1000);
    let opts = pump_options(&args.play, "history", stop_at);
    let result = pump::run(&mut events, &spec, &opts, stop_signal(args.play.seconds)).await;
    handle.stop().await;
    report(result.map(|s| (s, Some(params.end_s))))
}

fn report(result: Result<(pump::PumpStats, Option<u64>)>) -> Result<()> {
    let (stats, _) = result?;
    println!(
        "{} video frame(s), {} , {} audio frame(s), {} reconnect(s), stopped: {:?}",
        stats.frames,
        format_bytes(stats.bytes),
        stats.audio_frames,
        stats.reconnects,
        stats.stopped_by
    );
    if stats.frames == 0 {
        bail!("no video frames received");
    }
    Ok(())
}

// ----- recordings / download ---------------------------------------------------

async fn recordings(conn: &Connected, args: &RecordingsArgs) -> Result<()> {
    let mut filter = RecordingFilter {
        camera_ids: args.camera.into_iter().collect(),
        ..Default::default()
    }
    .page(args.offset, args.limit);
    if let Some(from) = &args.from {
        filter.from_time = Some(parse_local(from).context("--from")?);
    }
    if let Some(to) = &args.to {
        filter.to_time = Some(parse_local(to).context("--to")?);
    }
    let page = conn
        .client
        .list_recordings(&filter)
        .await
        .context("Recording.List")?;
    let mut table = Table::new([
        "id", "camera", "start", "stop", "length", "size", "type", "labels",
    ]);
    for r in &page.items {
        table.row([
            r.id.to_string(),
            format!("{} {}", r.camera_id, r.camera_name),
            format_local(r.start_time),
            if r.stop_time > r.start_time {
                format_local(r.stop_time)
            } else {
                "(recording)".into()
            },
            format_duration(r.duration_secs()),
            format_bytes(u64::try_from(r.file_size).unwrap_or(0)),
            r.event_type.to_string(),
            r.detection_labels().join(","),
        ]);
    }
    print!("{}", table.render());
    println!(
        "{} of {} recording(s) (offset {})",
        page.items.len(),
        page.total,
        args.offset
    );
    Ok(())
}

async fn download(conn: &Connected, args: &DownloadArgs) -> Result<()> {
    let mut file = tokio::fs::File::create(&args.out)
        .await
        .with_context(|| format!("creating {}", args.out.display()))?;
    eprintln!(
        "downloading recording {} to {}",
        args.recording_id,
        args.out.display()
    );
    let n = conn
        .client
        .download_recording(args.recording_id, &mut file)
        .await
        .context("Recording.Download")?;
    println!("{} written to {}", format_bytes(n), args.out.display());
    Ok(())
}

// ----- rtsp -------------------------------------------------------------------

async fn rtsp(conn: &Connected, args: &RtspArgs) -> Result<()> {
    let protocol: LiveProtocol = args.protocol.into();
    let url = conn
        .client
        .live_view_url(args.camera_id, protocol)
        .await
        .context("Camera.GetLiveViewPath")?;
    eprintln!(
        "[rtsp] camera {} {}: {}",
        args.camera_id,
        protocol.field(),
        redact(&url)
    );
    let mut player = MpvUrlPlayer::spawn(&url).context("starting mpv")?;
    let stop = stop_signal(args.seconds);
    tokio::pin!(stop);
    tokio::select! {
        r = player.wait_checked() => {
            r.context("mpv")?;
            println!("mpv exited");
        }
        () = &mut stop => {
            player.quit().await.context("quitting mpv")?;
            println!("mpv stopped");
        }
    }
    Ok(())
}
