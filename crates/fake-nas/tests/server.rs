//! The fake NAS driven by the real `ss_api::Client` and
//! `ss_stream::MixStreamClient`: proves its answers are what our parsers
//! accept. Skipped (with a message) when the media fixtures are missing.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fake_nas::{App, Config};
use ss_api::{CameraStatus, Client, Error, LoginOptions, RecordingFilter, SnapshotFilter};
use ss_stream::{
    Event, Frame, FrameKind, MixStreamClient, MixStreamConfig, MixStreamHandle, PlayParams,
    StreamProfile, TungsteniteConnector, VideoCodec, history_url, live_url,
};
use tokio::sync::mpsc::Receiver;
use tokio::time::{Instant, timeout_at};

struct Nas {
    base: String,
    _server: tokio::task::JoinHandle<()>,
}

async fn start(otp: Option<&str>) -> Option<Nas> {
    let cfg = Config {
        otp: otp.map(str::to_owned),
        ..Config::default()
    };
    if !cfg.fixtures.join("test.h264").exists() {
        eprintln!(
            "skipping: fixtures missing in {} (test.h264/test.aac/test.mp4)",
            cfg.fixtures.display()
        );
        return None;
    }
    let app = Arc::new(App::new(cfg).expect("fixtures load"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        fake_nas::serve(app, listener).await.unwrap();
    });
    Some(Nas {
        base: format!("http://{addr}"),
        _server: server,
    })
}

async fn login(nas: &Nas) -> Client {
    let c = Client::new(&nas.base).unwrap();
    assert!(c.discover_apis().await.unwrap() >= 10);
    c.login("admin", "admin", &LoginOptions::default())
        .await
        .unwrap();
    c
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// The next video frame before `deadline`, remembering any codec info.
async fn next_video(
    events: &mut Receiver<Event>,
    deadline: Instant,
    codec: &mut Option<VideoCodec>,
) -> Option<Frame> {
    loop {
        match timeout_at(deadline, events.recv()).await {
            Ok(Some(Event::Frame(f))) => match f.kind() {
                FrameKind::Video => return Some(f),
                FrameKind::CodecInfo { video, .. } => *codec = video,
                _ => {}
            },
            Ok(Some(Event::GaveUp(why))) => panic!("stream gave up: {why}"),
            Ok(Some(_)) => {}
            Ok(None) => panic!("event channel closed"),
            Err(_) => return None,
        }
    }
}

fn history(nas: &Nas, sid: &str, params: PlayParams) -> (MixStreamHandle, Receiver<Event>) {
    MixStreamClient::connect(
        MixStreamConfig::history(history_url(&nas.base).unwrap(), sid, params),
        TungsteniteConnector::new(),
    )
}

#[tokio::test]
async fn login_discovery_and_otp() {
    let Some(nas) = start(None).await else { return };
    let c = login(&nas).await;
    assert!(c.sid().is_some());
    assert_eq!(c.api_path("SYNO.API.Auth"), "/webapi/auth.cgi");

    let bad = Client::new(&nas.base).unwrap();
    let err = bad
        .login("admin", "wrong", &LoginOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Api(ref e) if e.code == 400), "{err:?}");

    // No session: a data call is a session error.
    let anon = Client::new(&nas.base).unwrap();
    anon.set_session(Some(ss_api::Session {
        sid: "nope".into(),
        did: None,
    }));
    assert!(matches!(
        anon.list_cameras().await.unwrap_err(),
        Error::SessionExpired(_)
    ));

    let Some(otp_nas) = start(Some("123456")).await else {
        return;
    };
    let o = Client::new(&otp_nas.base).unwrap();
    let err = o
        .login("admin", "admin", &LoginOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::OtpRequired(ref e) if e.code == 403),
        "{err:?}"
    );
    let err = o
        .login(
            "admin",
            "admin",
            &LoginOptions {
                otp_code: Some("000000".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::OtpRequired(ref e) if e.code == 404),
        "{err:?}"
    );
    let session = o
        .login(
            "admin",
            "admin",
            &LoginOptions {
                otp_code: Some("123456".into()),
                enable_device_token: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let did = session.did.expect("device token");
    // The trusted device skips the OTP next time.
    let again = Client::builder(&otp_nas.base)
        .device_id(did)
        .build()
        .unwrap();
    again
        .login("admin", "admin", &LoginOptions::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn cameras_and_paths() {
    let Some(nas) = start(None).await else { return };
    let c = login(&nas).await;
    let cams = c.list_cameras().await.unwrap();
    assert_eq!(cams.len(), 6);
    assert_eq!(cams.iter().filter(|c| c.status.is_streaming()).count(), 5);
    let front = &cams[0];
    assert_eq!((front.id, front.name.as_str()), (1, "Front Door"));
    assert!(front.is_ptz && front.has_audio);
    assert!(cams[1].video_codec.is_hevc(), "Driveway advertises H.265");
    let gate = cams.iter().find(|c| c.name == "Side Gate").unwrap();
    assert_eq!(gate.status, CameraStatus::Disabled);

    let info = c.camera_info(&[1, 4]).await.unwrap();
    assert_eq!(info.len(), 2);
    assert_eq!(info[1].def_live_profile, Some(ss_api::StreamProfile::Low));
    let paths = c.live_view_paths(&[1]).await.unwrap();
    assert!(paths[0].rtsp_path.starts_with("rtsp://"));
    let jpeg = c.camera_snapshot(1).await.unwrap();
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    assert!(matches!(c.camera_snapshot(6).await.unwrap_err(), Error::Api(ref e) if e.code == 402));
    c.ptz_move(1, ss_api::PtzDirection::LeftStart)
        .await
        .unwrap();
    assert_eq!(c.ptz_presets(1).await.unwrap().len(), 3);
}

#[tokio::test]
async fn enum_interval_covers_the_window() {
    let Some(nas) = start(None).await else { return };
    let c = login(&nas).await;
    let (from, to) = (now() - 7200, now());
    let cams = c.enum_interval(&[1, 2, 6], from, to).await.unwrap();
    assert_eq!(cams.len(), 3);
    for cam in cams.iter().filter(|c| c.camera_id != 6) {
        let spans = cam.presence_spans();
        assert_eq!(spans.len(), 1, "camera {}: {spans:?}", cam.camera_id);
        assert!(spans[0].0 <= from && spans[0].1 >= to - 5);
        assert!(cam.recordings.len() >= 4);
        let map = cam.event_map.as_array().expect("event_map array");
        let total: i64 = map.iter().map(|r| r[0].as_i64().unwrap()).sum();
        assert_eq!(total * 5, 7200);
        assert!(
            map.iter().any(|r| r[1].as_i64().unwrap() > 1),
            "no events in map"
        );
    }
    assert!(cams[2].presence_spans().is_empty());

    let events = c
        .list_event_center_events(&[1, 2, 3], from, to)
        .await
        .unwrap();
    assert!(!events.is_empty());
    assert!(
        events
            .windows(2)
            .all(|w| w[0].start_time >= w[1].start_time)
    );
    assert!(
        events
            .iter()
            .all(|e| e.start_time <= to && e.event_type > 0)
    );
    let cap = c.event_center_capability().await.unwrap();
    assert!(cap.event_types.contains(&10));
}

#[tokio::test]
async fn recordings_paginate_and_filter() {
    let Some(nas) = start(None).await else { return };
    let c = login(&nas).await;
    let p0 = c
        .list_recordings(&RecordingFilter::camera(1).page(0, 10))
        .await
        .unwrap();
    assert!((95..=97).contains(&p0.total), "{}", p0.total);
    assert_eq!(p0.items.len(), 10);
    assert!(
        p0.items
            .windows(2)
            .all(|w| w[0].start_time > w[1].start_time)
    );
    assert!(
        p0.items[0].stop_time >= now() - 5,
        "newest is the open file"
    );
    let p1 = c
        .list_recordings(&RecordingFilter::camera(1).page(10, 10))
        .await
        .unwrap();
    assert_eq!(p1.total, p0.total);
    assert!(
        p1.items
            .iter()
            .all(|r| !p0.items.iter().any(|s| s.id == r.id))
    );
    assert_eq!(p1.items[0].stop_time, p0.items[9].start_time);

    let window = c
        .list_recordings(
            &RecordingFilter::camera(2)
                .between(now() - 3 * 3600, now())
                .page(0, 50),
        )
        .await
        .unwrap();
    assert!(
        (6..=8).contains(&window.items.len()),
        "{}",
        window.items.len()
    );
    let all = c
        .list_recordings(&RecordingFilter::default().page(0, 5))
        .await
        .unwrap();
    assert_eq!(all.total, p0.total * 5);

    let mut out = Vec::new();
    let n = c
        .download_recording(p1.items[0].id, &mut out)
        .await
        .unwrap();
    assert!(n > 100_000 && out.starts_with(&[0, 0, 0]) || out[4..8] == *b"ftyp");
}

#[tokio::test]
async fn live_ws_delivers_video_within_3s() {
    let Some(nas) = start(None).await else { return };
    let c = login(&nas).await;
    let url = live_url(&nas.base, 1, StreamProfile::Balanced).unwrap();
    let (handle, mut events) = MixStreamClient::connect(
        MixStreamConfig::live(url, c.sid().unwrap()),
        TungsteniteConnector::new(),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut codec = None;
    let mut frames = Vec::new();
    while frames.len() < 10 {
        let f = next_video(&mut events, deadline, &mut codec)
            .await
            .unwrap_or_else(|| panic!("only {} video frames within 3 s", frames.len()));
        frames.push(f);
    }
    assert_eq!(codec, Some(VideoCodec::H264));
    let au = frames[0].annex_b();
    assert_eq!(&au[..4], &[0, 0, 0, 1]);
    // The first access unit is a keyframe carrying its parameter sets
    // (x264 puts an SEI first): SPS, PPS and an IDR slice are all in it.
    let nal_types: Vec<u8> = au
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == [0, 0, 0, 1])
        .map(|(i, _)| au[i + 4] & 0x1F)
        .collect();
    for t in [7, 8, 5] {
        assert!(
            nal_types.contains(&t),
            "NAL type {t} missing from first AU: {nal_types:?}"
        );
    }
    let msecs: Vec<u64> = frames.iter().filter_map(|f| f.header().msec()).collect();
    assert_eq!(msecs.len(), 10);
    assert!(msecs.windows(2).all(|w| w[1] > w[0]));
    handle.stop().await;
}

#[tokio::test]
async fn history_ws_plays_seeks_pauses_and_ends() {
    let Some(nas) = start(None).await else { return };
    let c = login(&nas).await;
    let sid = c.sid().unwrap();
    let page = c
        .list_recordings(&RecordingFilter::camera(2).page(0, 5))
        .await
        .unwrap();
    let rec = page
        .items
        .iter()
        .find(|r| r.duration_secs() >= 1790)
        .expect("a closed half-hour file");
    let dur = rec.duration_secs() as u64;
    let params = PlayParams::new(
        rec.id as u64,
        rec.mount_id,
        rec.arch_id,
        rec.event_type,
        100,
        dur,
    );
    let (handle, mut events) = history(&nas, &sid, params.clone());
    let mut codec = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    let first = next_video(&mut events, deadline, &mut codec)
        .await
        .expect("video after action=play");
    let m = first.header().msec().unwrap();
    assert!((99_000..106_000).contains(&m), "first msec {m}");
    assert_eq!(codec, Some(VideoCodec::H264));
    for _ in 0..4 {
        let f = next_video(&mut events, deadline, &mut codec).await.unwrap();
        assert!(f.header().msec().unwrap() >= m);
    }

    handle.seek_ms(600_000).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let f = next_video(&mut events, deadline, &mut codec)
            .await
            .expect("frames after seek");
        let m = f.header().msec().unwrap();
        if m >= 599_000 {
            assert!(m < 606_000, "seek landed at {m}");
            break;
        }
    }

    handle.pause().unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    while events.try_recv().is_ok() {}
    let quiet = next_video(
        &mut events,
        Instant::now() + Duration::from_secs(1),
        &mut codec,
    )
    .await;
    assert!(quiet.is_none(), "video kept flowing while paused");
    handle.resume().unwrap();
    let f = next_video(
        &mut events,
        Instant::now() + Duration::from_secs(3),
        &mut codec,
    )
    .await
    .expect("video after resume");
    assert!(f.header().msec().unwrap() >= 599_000);
    handle.stop().await;

    // Starting at the last second ends with a close notice.
    let mut at_end = params;
    at_end.start_s = dur - 1;
    let (handle, mut events) = history(&nas, &sid, at_end);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut closed = false;
    while let Ok(Some(ev)) = timeout_at(deadline, events.recv()).await {
        if let Event::Frame(f) = ev
            && f.kind() == FrameKind::Close
        {
            closed = true;
            break;
        }
    }
    assert!(closed, "no close notice at the end of the recording");
    handle.stop().await;
}

#[tokio::test]
async fn browse_endpoints_answer_sensibly() {
    let Some(nas) = start(None).await else { return };
    let c = login(&nas).await;
    let info = c.ss_info().await.unwrap();
    assert_eq!(info.version.unwrap().to_string(), "9.3.1-11180");
    assert_eq!(info.ds_model_name, "DS923+");
    assert_eq!(info.camera_number, 6);

    let alerts = c.list_alerts(0, 50).await.unwrap();
    assert_eq!((alerts.total, alerts.items.len()), (3, 3));
    assert_eq!(c.count_unread_alerts().await.unwrap(), 1);
    let unread: Vec<i64> = alerts
        .items
        .iter()
        .filter(|a| !a.is_read)
        .map(|a| a.id)
        .collect();
    c.mark_alerts_read(&unread).await.unwrap();
    assert_eq!(c.count_unread_alerts().await.unwrap(), 0);

    let lic = c.load_licenses().await.unwrap();
    assert_eq!(lic.licenses.len(), 1);
    c.add_licenses(&["TEST-KEY-0001"]).await.unwrap();
    let lic = c.load_licenses().await.unwrap();
    assert_eq!(lic.licenses.len(), 2);
    c.delete_licenses(&[lic.licenses[1].id]).await.unwrap();
    assert_eq!(c.load_licenses().await.unwrap().licenses.len(), 1);

    assert!(!c.home_mode().await.unwrap().on);
    c.set_home_mode(true).await.unwrap();
    assert!(c.home_mode().await.unwrap().on);

    let snaps = c.list_snapshots(&SnapshotFilter::default()).await.unwrap();
    assert_eq!((snaps.total, snaps.items.len()), (8, 8));
    assert!(!snaps.items[0].image_data.is_empty());
    let img = c.snapshot_image(snaps.items[0].id).await.unwrap();
    assert_eq!(&img[..2], &[0xFF, 0xD8]);
    let new_id = c.take_snapshot(3, 0).await.unwrap();
    assert!(new_id > 8);
    c.delete_snapshot(new_id, 0).await.unwrap();
    assert_eq!(
        c.list_snapshots(&SnapshotFilter::default())
            .await
            .unwrap()
            .total,
        8
    );

    // The UI's raw requests.
    let thumb = c
        .send(
            &ss_api::ApiRequest::new("SYNO.SurveillanceStation.Recording", "GetThumbnail", 5)
                .param("dsId", "0")
                .param("blFallbackByLoadEvt", "true")
                .param(
                    "eventInfo",
                    r#"[{"cameraId":1,"archId":0,"mountId":0,"rec_group":0,"targetTime":1}]"#,
                ),
        )
        .await
        .unwrap();
    assert!(
        thumb[0]["thumbnail"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    let tasks = c
        .send(&ss_api::ApiRequest::new(
            "SYNO.SurveillanceStation.TimeLapse",
            "ListTask",
            1,
        ))
        .await
        .unwrap();
    assert_eq!(tasks["task"].as_array().unwrap().len(), 2);
    let recs = c
        .send(
            &ss_api::ApiRequest::new("SYNO.SurveillanceStation.TimeLapse.Recording", "List", 1)
                .param("lapseId", -1)
                .param("start", 0)
                .param("limit", 100),
        )
        .await
        .unwrap();
    assert_eq!(recs["events"].as_array().unwrap().len(), 6);
}
