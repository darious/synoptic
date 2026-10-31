//! Wire-level tests against a wiremock server; nothing reaches a real NAS.

use serde_json::json;
use ss_api::{
    CameraStatus, Client, Error, LiveProtocol, LoginOptions, PtzDirection, RecordingFilter,
    Session, StreamProfile,
};
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const ENTRY: &str = "/webapi/entry.cgi";
const AUTH: &str = "/webapi/auth.cgi";

fn ok(data: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"success": true, "data": data}))
}

fn fail(code: i64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"success": false, "error": {"code": code}}))
}

async fn server_with_info() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/webapi/query.cgi"))
        .and(query_param("api", "SYNO.API.Info"))
        .and(query_param("method", "Query"))
        .and(query_param("query", "all"))
        .and(query_param("version", "1"))
        .respond_with(ok(json!({
            "SYNO.API.Auth": {"path": "auth.cgi", "minVersion": 1, "maxVersion": 7},
            "SYNO.SurveillanceStation.Camera": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 9},
            "SYNO.SurveillanceStation.Recording": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 6},
            "SYNO.SurveillanceStation.Stream": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 1}
        })))
        .mount(&server)
        .await;
    server
}

fn client(server: &MockServer) -> Client {
    Client::builder(server.uri()).build().unwrap()
}

fn logged_in(server: &MockServer) -> Client {
    let c = client(server);
    c.set_session(Some(Session {
        sid: "test-sid".into(),
        did: None,
    }));
    c
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap_or_default()
}

// ----- discovery -----------------------------------------------------------

#[tokio::test]
async fn discovery_populates_table_and_resolves_paths() {
    let server = server_with_info().await;
    let c = client(&server);
    assert_eq!(c.api_path("SYNO.API.Auth"), "/webapi/entry.cgi");

    assert_eq!(c.discover_apis().await.unwrap(), 4);
    assert!(c.has_api("SYNO.API.Auth"));
    assert_eq!(c.api_info("SYNO.API.Auth").unwrap().max_version, 7);
    assert_eq!(c.api_info("SYNO.API.Auth").unwrap().min_version, 1);
    assert_eq!(c.api_path("SYNO.API.Auth"), "/webapi/auth.cgi");
    assert_eq!(c.api_path("SYNO.Unknown"), "/webapi/entry.cgi");
    assert!(!c.has_api("SYNO.Unknown"));
}

#[tokio::test]
async fn versions_are_clamped_like_the_reference() {
    let server = server_with_info().await;
    let c = client(&server);
    c.discover_apis().await.unwrap();
    assert_eq!(
        c.api_version("SYNO.SurveillanceStation.Camera", Some(99)),
        9
    );
    assert_eq!(c.api_version("SYNO.SurveillanceStation.Camera", Some(3)), 3);
    assert_eq!(c.api_version("SYNO.SurveillanceStation.Camera", None), 9);
    assert_eq!(c.api_version("SYNO.Unknown", Some(5)), 5);
    assert_eq!(c.api_version("SYNO.Unknown", None), 1);

    Mock::given(method("GET"))
        .and(path(ENTRY))
        .and(query_param("api", "SYNO.SurveillanceStation.Camera"))
        .and(query_param("version", "9"))
        .respond_with(ok(json!({"cameras": []})))
        .mount(&server)
        .await;
    c.request("SYNO.SurveillanceStation.Camera", "List", 99, &[])
        .await
        .unwrap();
}

#[tokio::test]
async fn discovery_failure_uses_common_table() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/webapi/query.cgi"))
        .respond_with(fail(102))
        .mount(&server)
        .await;
    let err = client(&server).discover_apis().await.unwrap_err();
    assert_eq!(err.code(), Some(102));
    assert_eq!(err.api_error().unwrap().message, "API does not exist");
}

// ----- login -----------------------------------------------------------------

#[tokio::test]
async fn login_posts_credentials_in_body_and_stores_session() {
    let server = server_with_info().await;
    Mock::given(method("POST"))
        .and(path(AUTH))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string_contains("api=SYNO.API.Auth"))
        .and(body_string_contains("method=Login"))
        .and(body_string_contains("version=6"))
        .and(body_string_contains("account=alice"))
        .and(body_string_contains("passwd=s3cret%21"))
        .and(body_string_contains("session=SurveillanceStation"))
        .and(body_string_contains("format=sid"))
        .and(body_string_contains("otp_code=123456"))
        .and(body_string_contains("enable_device_token=yes"))
        .and(body_string_contains("device_name=laptop"))
        .respond_with(ok(json!({"sid": "new-sid", "did": "trusted-device"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(ENTRY))
        .and(query_param("_sid", "new-sid"))
        .respond_with(ok(json!({"cameras": []})))
        .mount(&server)
        .await;

    let c = client(&server);
    c.discover_apis().await.unwrap();
    let options = LoginOptions {
        otp_code: Some("123456".into()),
        device_name: Some("laptop".into()),
        enable_device_token: true,
        ..Default::default()
    };
    let session = c.login("alice", "s3cret!", &options).await.unwrap();
    assert_eq!(session.sid, "new-sid");
    assert_eq!(session.did.as_deref(), Some("trusted-device"));
    assert_eq!(c.sid().as_deref(), Some("new-sid"));
    assert_eq!(c.device_id().as_deref(), Some("trusted-device"));
    assert!(!c.can_relogin());

    // The next call carries _sid and nothing secret went into a URL.
    c.list_cameras().await.unwrap();
    for req in requests(&server).await {
        let url = req.url.as_str();
        assert!(!url.contains("s3cret") && !url.contains("123456"), "{url}");
        if req.method == "POST" {
            assert!(!url.contains("new-sid"));
            assert!(!req.url.query().unwrap_or("").contains("passwd"));
        }
    }
}

#[tokio::test]
async fn login_presents_stored_device_id_and_can_omit_optionals() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(ENTRY))
        .respond_with(ok(json!({"sid": "sid2"})))
        .mount(&server)
        .await;
    let c = Client::builder(server.uri())
        .device_id("from-profile")
        .build()
        .unwrap();
    c.login("bob", "pw", &LoginOptions::default())
        .await
        .unwrap();
    let body = String::from_utf8(requests(&server).await[0].body.clone()).unwrap();
    assert!(body.contains("device_id=from-profile"), "{body}");
    assert!(!body.contains("otp_code") && !body.contains("enable_device_token"));
    assert!(!body.contains("device_name"));
    assert_eq!(c.session().unwrap().did, None);
}

#[tokio::test]
async fn login_without_sid_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok(json!({})))
        .mount(&server)
        .await;
    let err = client(&server)
        .login("a", "b", &LoginOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::MissingSid));
}

#[tokio::test]
async fn otp_required_is_typed_only_under_auth() {
    for (code, text) in [
        (403, "Two-factor authentication required"),
        (404, "Invalid OTP code"),
        (406, "Two-factor authentication enforced"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(fail(code))
            .mount(&server)
            .await;
        let err = client(&server)
            .login("a", "b", &LoginOptions::default())
            .await
            .unwrap_err();
        match err {
            Error::OtpRequired(e) => {
                assert_eq!(e.code, code);
                assert_eq!(e.message, text);
            }
            other => panic!("expected OtpRequired, got {other:?}"),
        }
    }

    // 403 from Surveillance Station is not a two-factor prompt.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(fail(403))
        .mount(&server)
        .await;
    let err = logged_in(&server).list_cameras().await.unwrap_err();
    assert!(matches!(err, Error::Api(ref e) if e.code == 403));
    assert_eq!(err.api_error().unwrap().message, "Unknown error (403)");
}

#[tokio::test]
async fn bad_password_uses_auth_table() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"success": false, "error": {"code": 400}})),
        )
        .mount(&server)
        .await;
    let err = client(&server)
        .login("a", "b", &LoginOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Api(_)));
    assert_eq!(
        err.api_error().unwrap().message,
        "No such account or incorrect password"
    );
    assert_eq!(
        err.to_string(),
        "No such account or incorrect password (code 400)"
    );
}

#[tokio::test]
async fn logout_sends_request_and_clears_everything() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ENTRY))
        .and(query_param("api", "SYNO.API.Auth"))
        .and(query_param("method", "Logout"))
        .and(query_param("version", "6"))
        .and(query_param("session", "SurveillanceStation"))
        .and(query_param("_sid", "test-sid"))
        .respond_with(ok(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let c = logged_in(&server);
    c.logout().await;
    assert!(c.session().is_none());
    assert!(!c.can_relogin());
    // Without a session nothing is sent.
    c.logout().await;
}

// ----- session errors --------------------------------------------------------

#[tokio::test]
async fn session_errors_are_typed() {
    for code in [105, 106, 107, 119] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(fail(code))
            .mount(&server)
            .await;
        let err = logged_in(&server).list_cameras().await.unwrap_err();
        assert!(
            matches!(err, Error::SessionExpired(ref e) if e.code == code),
            "{code}: {err:?}"
        );
    }
}

#[tokio::test]
async fn html_login_page_is_a_session_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html><body>Login</body></html>")
                .insert_header("content-type", "text/html"),
        )
        .mount(&server)
        .await;
    let err = logged_in(&server).list_cameras().await.unwrap_err();
    assert!(
        matches!(err, Error::SessionExpired(ref e) if e.code == 119),
        "{err:?}"
    );
}

#[tokio::test]
async fn auto_relogin_retries_once_with_remembered_credentials() {
    let server = MockServer::start().await;
    // Login (twice: the initial one and the re-login).
    Mock::given(method("POST"))
        .and(body_string_contains("method=Login"))
        .and(body_string_contains("account=alice"))
        .and(body_string_contains("passwd=pw"))
        .respond_with(ok(json!({"sid": "sid-1"})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("method=Login"))
        .and(body_string_contains("device_id=dev"))
        .and(body_string_contains("device_name=host"))
        .respond_with(ok(json!({"sid": "sid-2"})))
        .mount(&server)
        .await;
    // First data call: session gone. Second: fine, with the new sid.
    Mock::given(method("GET"))
        .and(query_param("_sid", "sid-1"))
        .respond_with(fail(119))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("_sid", "sid-2"))
        .respond_with(ok(
            json!({"cameras": [{"id": 1, "name": "A", "status": 1}]}),
        ))
        .mount(&server)
        .await;

    let c = Client::builder(server.uri())
        .device_id("dev")
        .build()
        .unwrap();
    let options = LoginOptions {
        device_name: Some("host".into()),
        remember_credentials: true,
        ..Default::default()
    };
    c.login("alice", "pw", &options).await.unwrap();
    assert!(c.can_relogin());
    let cams = c.list_cameras().await.unwrap();
    assert_eq!(cams.len(), 1);
    assert_eq!(c.sid().as_deref(), Some("sid-2"));
    let posts = requests(&server)
        .await
        .iter()
        .filter(|r| r.method == "POST")
        .count();
    assert_eq!(posts, 2);
}

#[tokio::test]
async fn relogin_failure_is_reported_with_its_cause() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok(json!({"sid": "sid-1"})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(fail(400))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(fail(105))
        .mount(&server)
        .await;

    let c = client(&server);
    let options = LoginOptions {
        remember_credentials: true,
        ..Default::default()
    };
    c.login("alice", "stale", &options).await.unwrap();
    let err = c.list_cameras().await.unwrap_err();
    match err {
        Error::ReloginFailed { session, cause } => {
            assert_eq!(session.code, 105);
            assert_eq!(cause.code(), Some(400));
        }
        other => panic!("expected ReloginFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn no_relogin_without_remembered_credentials() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(fail(119))
        .expect(1)
        .mount(&server)
        .await;
    let c = logged_in(&server);
    let err = c.list_cameras().await.unwrap_err();
    assert!(matches!(err, Error::SessionExpired(_)));
    // The stale session is left for the caller to replace.
    assert!(c.session().is_some());
}

// ----- transport / HTTP errors carry no secrets -----------------------------

#[tokio::test]
async fn http_status_error_has_no_url_or_sid() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let err = logged_in(&server).list_cameras().await.unwrap_err();
    assert!(matches!(err, Error::HttpStatus(500)));
    let msg = err.to_string();
    assert_eq!(msg, "server returned HTTP 500 Internal Server Error");
    assert!(!msg.contains("test-sid") && !msg.contains("127.0.0.1"));
}

#[tokio::test]
async fn transport_error_has_no_url_or_sid() {
    // Port 1 on loopback: nothing listens there, and unlike a dropped
    // MockServer's port no parallel test can grab it in the meantime.
    let c = Client::builder("http://127.0.0.1:1")
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    c.set_session(Some(Session {
        sid: "test-sid".into(),
        did: None,
    }));
    let err = c.list_cameras().await.unwrap_err();
    assert!(matches!(err, Error::Transport(_)), "{err:?}");
    let msg = format!("{err} / {err:?}");
    assert!(!msg.contains("test-sid") && !msg.contains("_sid"), "{msg}");
}

// ----- cameras ---------------------------------------------------------------

#[tokio::test]
async fn camera_list_sends_reference_params_and_parses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(ENTRY))
        .and(query_param("api", "SYNO.SurveillanceStation.Camera"))
        .and(query_param("method", "List"))
        .and(query_param("version", "9"))
        .and(query_param("basic", "true"))
        .and(query_param("streamInfo", "true"))
        .and(query_param("ptz", "true"))
        .and(query_param("_sid", "test-sid"))
        .respond_with(ok(json!({"cameras": [
            {"id": 1, "newName": "Front Door", "name": "cam1", "model": "DS-2CD", "vendor": "Hikvision",
             "status": 1, "ptzDirection": 1, "audioCodec": 4, "audioOut": true},
            {"id": "2", "name": "Back Yard", "status": "7"},
            {"id": 3, "name": "Garage", "status": 3, "audioType": 0, "audioOut": false},
            {"id": 4, "name": "Gone", "status": 2}
        ], "total": 4})))
        .mount(&server)
        .await;
    let cams = logged_in(&server).list_cameras().await.unwrap();
    assert_eq!(cams.len(), 4);
    assert_eq!(cams[0].name, "Front Door");
    assert_eq!(cams[0].status, CameraStatus::Enabled);
    assert!(cams[0].is_ptz && cams[0].has_audio && cams[0].has_speaker);
    assert_eq!(cams[1].id, 2);
    assert_eq!(cams[1].name, "Back Yard");
    assert_eq!(cams[1].status, CameraStatus::Disabled);
    assert_eq!(cams[2].status, CameraStatus::Disconnected);
    assert!(!cams[2].has_audio && !cams[2].has_speaker);
    assert_eq!(cams[3].status, CameraStatus::Other(2));
    assert!(!cams[3].status.is_streaming());
}

#[tokio::test]
async fn live_view_profile_comes_from_get_info() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("api", "SYNO.SurveillanceStation.Camera"))
        .and(query_param("method", "GetInfo"))
        .and(query_param("version", "8"))
        .and(query_param("cameraIds", "7"))
        .and(query_param("streamInfo", "true"))
        .respond_with(ok(json!({"cameras": [{"id": 7, "defLiveProfile": 2}]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("method", "GetInfo"))
        .and(query_param("cameraIds", "8"))
        .respond_with(ok(json!({"cameras": [{"id": 8, "defLiveProfile": 5}]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("method", "GetInfo"))
        .and(query_param("cameraIds", "9"))
        .respond_with(fail(400))
        .mount(&server)
        .await;
    let c = logged_in(&server);
    assert_eq!(c.live_view_profile(7).await.unwrap(), StreamProfile::Low);
    assert_eq!(
        c.live_view_profile(8).await.unwrap(),
        StreamProfile::Balanced
    );
    assert!(c.live_view_profile(9).await.is_err());
    assert_eq!(
        c.live_view_profile(9).await.unwrap_or_default(),
        StreamProfile::Balanced
    );
}

#[tokio::test]
async fn live_view_path_selection() {
    let server = MockServer::start().await;
    // Camera 1 answers with a bare list; camera 2 with {pathInfos: [...]}.
    Mock::given(method("GET"))
        .and(query_param("api", "SYNO.SurveillanceStation.Camera"))
        .and(query_param("method", "GetLiveViewPath"))
        .and(query_param("version", "9"))
        .and(query_param("idList", "1"))
        .respond_with(ok(json!([{
            "id": 1,
            "rtspPath": "rtsp://nas:554/Sms=1.unicast",
            "rtspOverHttpPath": "http://nas:5000/webapi/rtsp?id=1",
            "mjpegHttpPath": "/webapi/entry.cgi?api=SYNO.SurveillanceStation.Stream&id=1",
            "multicstPath": ""
        }])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("method", "GetLiveViewPath"))
        .and(query_param("idList", "2"))
        .respond_with(ok(json!({"pathInfos": [{
            "id": 2, "rtspPath": "", "mjpegHttpPath": "http://nas:5000/mjpeg?id=2"
        }]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("method", "GetLiveViewPath"))
        .and(query_param("idList", "3"))
        .respond_with(ok(json!({"pathInfos": []})))
        .mount(&server)
        .await;

    let c = logged_in(&server);
    assert_eq!(
        c.live_view_url(1, LiveProtocol::Rtsp).await.unwrap(),
        "rtsp://nas:554/Sms=1.unicast"
    );
    assert_eq!(
        c.live_view_url(1, LiveProtocol::RtspOverHttp)
            .await
            .unwrap(),
        "http://nas:5000/webapi/rtsp?id=1"
    );
    assert_eq!(
        c.live_view_url(1, LiveProtocol::Mjpeg).await.unwrap(),
        format!(
            "{}/webapi/entry.cgi?api=SYNO.SurveillanceStation.Stream&id=1",
            server.uri()
        )
    );
    assert!(matches!(
        c.live_view_url(1, LiveProtocol::Multicast).await,
        Err(Error::InvalidResponse(_))
    ));
    assert_eq!(
        c.live_view_url(2, LiveProtocol::Mjpeg).await.unwrap(),
        "http://nas:5000/mjpeg?id=2"
    );
    assert!(matches!(
        c.live_view_url(2, LiveProtocol::Rtsp).await,
        Err(Error::InvalidResponse(_))
    ));
    assert!(matches!(
        c.live_view_url(3, LiveProtocol::Rtsp).await,
        Err(Error::InvalidResponse(_))
    ));
    let paths = c.live_view_paths(&[1]).await.unwrap();
    assert_eq!(paths[0].path(LiveProtocol::Multicast), None);
}

// ----- recordings ------------------------------------------------------------

#[tokio::test]
async fn recording_list_params_and_parsing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("api", "SYNO.SurveillanceStation.Recording"))
        .and(query_param("method", "List"))
        .and(query_param("version", "5"))
        .and(query_param("cameraIds", "1,2"))
        .and(query_param("fromTime", "100"))
        .and(query_param("toTime", "200"))
        .and(query_param("offset", "10"))
        .and(query_param("limit", "5"))
        .respond_with(ok(json!({
            "events": [{"id": 100, "cameraId": 1, "camera_name": "Front", "startTime": 110,
                         "stopTime": 150, "fileSize": "2048", "type": 1, "mountId": 0, "archId": 3,
                         "defaultLabel": 2}],
            "total": "42"
        })))
        .mount(&server)
        .await;
    let filter = RecordingFilter {
        camera_ids: vec![1, 2],
        ..Default::default()
    }
    .between(100, 200)
    .page(10, 5);
    let page = logged_in(&server).list_recordings(&filter).await.unwrap();
    assert_eq!(page.total, 42);
    assert_eq!(page.items.len(), 1);
    let rec = &page.items[0];
    assert_eq!(rec.camera_name, "Front");
    assert_eq!(rec.file_size, 2048);
    assert_eq!(rec.arch_id, 3);
    assert_eq!(rec.detection_labels(), vec!["Person"]);
}

#[tokio::test]
async fn recording_list_all_pages_until_total() {
    let server = MockServer::start().await;
    // page 1: 200 items (offset 0), page 2: 50 items (offset 200), total 250
    let item = |id: i64| {
        json!({"id": id, "cameraId": 3, "startTime": 1000 + id, "stopTime": 1100 + id,
               "type": 1, "mountId": 0, "archId": 0})
    };
    let first: Vec<_> = (0..200).map(item).collect();
    let second: Vec<_> = (200..250).map(item).collect();
    Mock::given(method("GET"))
        .and(query_param("api", "SYNO.SurveillanceStation.Recording"))
        .and(query_param("method", "List"))
        .and(query_param("cameraIds", "3"))
        .and(query_param("offset", "0"))
        .and(query_param("limit", "200"))
        .respond_with(ok(json!({"events": first, "total": 250})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("api", "SYNO.SurveillanceStation.Recording"))
        .and(query_param("method", "List"))
        .and(query_param("offset", "200"))
        .and(query_param("limit", "200"))
        .respond_with(ok(json!({"events": second, "total": 250})))
        .mount(&server)
        .await;
    let filter = RecordingFilter::camera(3).between(0, 5000).page(0, 50);
    let all = logged_in(&server)
        .list_recordings_all(&filter)
        .await
        .unwrap();
    assert_eq!(all.len(), 250);
    assert_eq!(all.first().map(|r| r.id), Some(0));
    assert_eq!(all.last().map(|r| r.id), Some(249));
    // exactly two requests: the small requested page size was raised to 200
    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn recording_download_streams_bytes_with_v6() {
    let server = server_with_info().await;
    let body: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    Mock::given(method("GET"))
        .and(path(ENTRY))
        .and(query_param("api", "SYNO.SurveillanceStation.Recording"))
        .and(query_param("method", "Download"))
        .and(query_param("version", "6"))
        .and(query_param("id", "100"))
        .and(query_param("_sid", "test-sid"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body.clone())
                .insert_header("content-type", "video/mp4"),
        )
        .mount(&server)
        .await;
    let c = logged_in(&server);
    c.discover_apis().await.unwrap();
    let mut out = Vec::new();
    let n = c.download_recording(100, &mut out).await.unwrap();
    assert_eq!(n, 5000);
    assert_eq!(out, body);
}

#[tokio::test]
async fn small_download_is_delivered_whole() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\xff\xd8\xff\xe0tiny".to_vec()))
        .mount(&server)
        .await;
    let bytes = logged_in(&server).snapshot_image(5).await.unwrap();
    assert_eq!(&bytes[..], b"\xff\xd8\xff\xe0tiny");
}

#[tokio::test]
async fn download_errors_are_detected_before_writing() {
    // JSON error with a JSON content type.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(fail(400))
        .mount(&server)
        .await;
    let mut out = Vec::new();
    let err = logged_in(&server)
        .download_recording(1, &mut out)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(400));
    assert!(out.is_empty());

    // HTML login page.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html>login</html>")
                .insert_header("content-type", "text/html; charset=utf-8"),
        )
        .mount(&server)
        .await;
    let err = logged_in(&server)
        .download_recording(1, &mut out)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::SessionExpired(ref e) if e.code == 119));
    assert!(out.is_empty());

    // HTML without a content type: caught by sniffing the head.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_bytes(b"\n<!DOCTYPE html><html>".to_vec()),
        )
        .mount(&server)
        .await;
    let err = logged_in(&server)
        .download_recording(1, &mut out)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::SessionExpired(_)));
    assert!(out.is_empty());

    // JSON error without a content type.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(br#"{"success":false,"error":{"code":402}}"#.to_vec()),
        )
        .mount(&server)
        .await;
    let err = logged_in(&server)
        .download_recording(1, &mut out)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(402));
    assert!(out.is_empty());

    // Empty body.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let err = logged_in(&server)
        .download_recording(1, &mut out)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(100));
    assert!(out.is_empty());

    // HTTP error.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let err = logged_in(&server)
        .download_recording(1, &mut out)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::HttpStatus(404)));
}

#[tokio::test]
async fn download_relogins_when_session_is_gone() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok(json!({"sid": "sid-1"})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ok(json!({"sid": "sid-2"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("_sid", "sid-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html>login</html>")
                .insert_header("content-type", "text/html"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("_sid", "sid-2"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"video".to_vec()))
        .mount(&server)
        .await;
    let c = client(&server);
    let options = LoginOptions {
        remember_credentials: true,
        ..Default::default()
    };
    c.login("a", "b", &options).await.unwrap();
    let mut out = Vec::new();
    assert_eq!(c.download_recording(1, &mut out).await.unwrap(), 5);
    assert_eq!(out, b"video");
}

#[tokio::test]
async fn recording_stream_url_prefers_modern_api() {
    let server = server_with_info().await;
    let c = logged_in(&server);
    c.discover_apis().await.unwrap();
    let rec: ss_api::Recording = serde_json::from_value(json!({
        "id": 9, "cameraId": 1, "startTime": 1, "stopTime": 2, "mountId": 3, "archId": 4, "type": 5
    }))
    .unwrap();
    let url = c.recording_stream_url(&rec);
    let s = url.as_str();
    assert!(s.starts_with(&format!("{}/webapi/entry.cgi?_sid=test-sid", server.uri())));
    assert!(s.contains("api=SYNO.SurveillanceStation.Stream&"));
    assert!(s.contains("eventId=9") && s.contains("mountId=3"));
    assert!(s.contains("recEvtType=5") && s.contains("archId=4"));
}

// ----- snapshots, PTZ, info ----------------------------------------------------

#[tokio::test]
async fn take_snapshot_coerces_string_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("api", "SYNO.SurveillanceStation.SnapShot"))
        .and(query_param("method", "TakeSnapshot"))
        .and(query_param("version", "1"))
        .and(query_param("dsId", "0"))
        .and(query_param("camId", "3"))
        .and(query_param("blSave", "true"))
        .respond_with(ok(json!({"id": "77"})))
        .mount(&server)
        .await;
    assert_eq!(logged_in(&server).take_snapshot(3, 0).await.unwrap(), 77);
}

#[tokio::test]
async fn snapshot_list_uses_official_param_names() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("method", "List"))
        .and(query_param("start", "0"))
        .and(query_param("limit", "50"))
        .and(query_param("camIdList", "2"))
        .and(query_param("from", "10"))
        .and(query_param("to", "20"))
        .respond_with(ok(json!({"data": [
            {"id": 50, "camName": "Garage", "createdTm": 1_700_001_000, "imageData": "Zm9v"}
        ], "total": 1})))
        .mount(&server)
        .await;
    let filter = ss_api::SnapshotFilter {
        camera_id: Some(2),
        from_time: Some(10),
        to_time: Some(20),
        ..Default::default()
    };
    let page = logged_in(&server).list_snapshots(&filter).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].camera_name, "Garage");
}

#[tokio::test]
async fn ptz_move_sends_direction() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("api", "SYNO.SurveillanceStation.PTZ"))
        .and(query_param("method", "Move"))
        .and(query_param("version", "2"))
        .and(query_param("cameraId", "4"))
        .and(query_param("direction", "leftStart"))
        .respond_with(ok(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    logged_in(&server)
        .ptz_move(4, PtzDirection::LeftStart)
        .await
        .unwrap();
}

#[tokio::test]
async fn ss_info_reports_version() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("api", "SYNO.SurveillanceStation.Info"))
        .and(query_param("method", "GetInfo"))
        .and(query_param("version", "8"))
        .respond_with(ok(json!({
            "version": {"major": "9", "minor": "2", "small": "1", "build": "11575"},
            "serial": "2130ABC", "DSModelName": "DS224+", "cameraNumber": 4,
            "liscenseNumber": 2, "maxCameraSupport": 25, "isLicenseEnough": true
        })))
        .mount(&server)
        .await;
    let info = logged_in(&server).ss_info().await.unwrap();
    assert_eq!(info.version.unwrap().to_string(), "9.2.1-11575");
    assert_eq!(info.serial, "2130ABC");
    assert_eq!(info.ds_model_name, "DS224+");
    assert_eq!(info.max_camera_support, 25);
}

#[tokio::test]
async fn event_center_list_pages_until_empty() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("api", "SYNO.SurveillanceStation.EventCenter.Event"))
        .and(query_param("method", "List"))
        .and(query_param("camera_ids", "1,2"))
        .and(query_param("start_time", "0"))
        .and(query_param("end_time", "1000"))
        .and(query_param("start_time_offset", r#"{"0":{"id":0,"start_time":0}}"#))
        .respond_with(ok(json!({"0": [
            {"id": 5, "camera_id": 1, "start_time": 900, "duration": 4, "event_type": 2, "object_type": 0},
            {"id": 4, "camera_id": 2, "start_time": 800, "duration": 0, "event_type": 5, "object_type": 2}
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("method", "List"))
        .and(query_param(
            "start_time_offset",
            r#"{"0":{"id":4,"start_time":800}}"#,
        ))
        .respond_with(ok(json!({"0": []})))
        .mount(&server)
        .await;
    let events = logged_in(&server)
        .list_event_center_events(&[1, 2], 0, 1000)
        .await
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].event_type, 5);
}
