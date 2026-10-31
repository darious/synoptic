//! `ss cameras` end to end against a wiremock stand-in for the NAS:
//! `SYNO.API.Info` discovery, `SYNO.API.Auth` login, `Camera.List`,
//! `Camera.GetInfo`, logout. Nothing reaches a real NAS and no secret may
//! appear in the binary's output.

use std::path::Path;
use std::process::Output;

use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PASSWORD: &str = "s3cret-pw";
const SID: &str = "sid-0123456789";

fn ok(data: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"success": true, "data": data}))
}

fn fail(code: i64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"success": false, "error": {"code": code}}))
}

/// Discovery + logout mocks; the login answer is the caller's.
async fn fake_nas(login: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/webapi/query.cgi"))
        .and(query_param("api", "SYNO.API.Info"))
        .and(query_param("method", "Query"))
        .respond_with(ok(json!({
            "SYNO.API.Auth": {"path": "auth.cgi", "minVersion": 1, "maxVersion": 7},
            "SYNO.SurveillanceStation.Camera": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 9},
            "SYNO.SurveillanceStation.Info": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 8},
            "SYNO.SurveillanceStation.Recording": {"path": "entry.cgi", "minVersion": 1, "maxVersion": 6}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/webapi/auth.cgi"))
        .and(body_string_contains("method=Login"))
        .and(body_string_contains("account=alice"))
        .and(body_string_contains(format!("passwd={PASSWORD}")))
        .and(body_string_contains("session=SurveillanceStation"))
        .and(body_string_contains("format=sid"))
        .respond_with(login)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/webapi/auth.cgi"))
        .and(query_param("method", "Logout"))
        .respond_with(ok(json!({})))
        .mount(&server)
        .await;
    server
}

async fn run_ss(server: &MockServer, config_home: &Path, args: &[&str]) -> Output {
    let port = server.address().port().to_string();
    tokio::process::Command::new(env!("CARGO_BIN_EXE_ss"))
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port,
            "--no-https",
            "--user",
            "alice",
        ])
        .args(args)
        .env("SS_PASSWORD", PASSWORD)
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("RUST_LOG")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .expect("run the ss binary")
}

fn text(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test]
async fn cameras_lists_the_fake_nas() {
    let server = fake_nas(ok(json!({"sid": SID}))).await;
    Mock::given(method("GET"))
        .and(path("/webapi/entry.cgi"))
        .and(query_param("api", "SYNO.SurveillanceStation.Camera"))
        .and(query_param("method", "List"))
        .and(query_param("version", "9"))
        .and(query_param("basic", "true"))
        .and(query_param("streamInfo", "true"))
        .and(query_param("ptz", "true"))
        .and(query_param("_sid", SID))
        .respond_with(ok(json!({"cameras": [
            {"id": 1, "newName": "Front Door", "name": "cam1", "model": "DS-2CD2386G2",
             "vendor": "Hikvision", "status": 1, "ptzDirection": 1, "audioCodec": 4,
             "videoCodec": 3},
            {"id": 2, "name": "Garage", "model": "C200", "vendor": "TP-Link", "status": 7,
             "videoCodec": "5"},
            {"id": 3, "name": "Yard", "status": 3}
        ], "total": 3})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/webapi/entry.cgi"))
        .and(query_param("api", "SYNO.SurveillanceStation.Camera"))
        .and(query_param("method", "GetInfo"))
        .and(query_param("cameraIds", "1,2,3"))
        .and(query_param("_sid", SID))
        .respond_with(ok(json!({"cameras": [
            {"id": 1, "defLiveProfile": 0},
            {"id": 2, "defLiveProfile": 2},
            {"id": 3}
        ]})))
        .expect(1)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let out = run_ss(&server, dir.path(), &["cameras"]).await;
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");

    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines[0].starts_with("id  name"), "{stdout}");
    let front = lines
        .iter()
        .find(|l| l.contains("Front Door"))
        .expect("camera 1 row");
    for cell in ["Hikvision", "DS-2CD2386G2", "enabled", "H.264", "high"] {
        assert!(front.contains(cell), "{front}");
    }
    let garage = lines
        .iter()
        .find(|l| l.contains("Garage"))
        .expect("camera 2 row");
    for cell in ["TP-Link", "disabled", "H.265", "low"] {
        assert!(garage.contains(cell), "{garage}");
    }
    let yard = lines
        .iter()
        .find(|l| l.contains("Yard"))
        .expect("camera 3 row");
    assert!(yard.contains("disconnected"), "{yard}");
    assert!(stdout.contains("3 camera(s)"), "{stdout}");

    // Neither the session id nor the password leaks anywhere.
    for secret in [SID, PASSWORD] {
        assert!(!stdout.contains(secret), "{stdout}");
        assert!(!stderr.contains(secret), "{stderr}");
    }
    // Nothing was written to the config: `cameras` only reads it.
    assert!(!dir.path().join("surveillance-station").exists());
    server.verify().await;
}

#[tokio::test]
async fn otp_required_exits_3_with_a_hint() {
    let server = fake_nas(fail(403)).await;
    let dir = tempfile::tempdir().unwrap();
    let out = run_ss(&server, dir.path(), &["cameras"]).await;
    let (stdout, stderr) = text(&out);
    assert_eq!(
        out.status.code(),
        Some(3),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stderr.contains("two-factor"), "{stderr}");
    assert!(stderr.contains("--otp"), "{stderr}");
    assert!(!stderr.contains(PASSWORD));
}

#[tokio::test]
async fn bad_password_exits_1_with_the_auth_table_message() {
    let server = fake_nas(fail(400)).await;
    let dir = tempfile::tempdir().unwrap();
    let out = run_ss(&server, dir.path(), &["cameras"]).await;
    let (_, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("incorrect password"), "{stderr}");
    assert!(!stderr.contains(PASSWORD));
}

#[tokio::test]
async fn login_saves_a_profile_and_reports_the_version() {
    let server = fake_nas(ok(json!({"sid": SID, "did": "trusted-token"}))).await;
    Mock::given(method("GET"))
        .and(path("/webapi/entry.cgi"))
        .and(query_param("api", "SYNO.SurveillanceStation.Info"))
        .and(query_param("method", "GetInfo"))
        .and(query_param("_sid", SID))
        .respond_with(ok(json!({
            "version": {"major": "9", "minor": "2", "small": "1", "build": "11575"},
            "DSModelName": "DS224+", "hostname": "nas", "cameraNumber": 3, "liscenseNumber": 2
        })))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let out = run_ss(
        &server,
        dir.path(),
        &[
            "login",
            "--profile",
            "test",
            "--trust-device",
            "--no-keyring",
        ],
    )
    .await;
    let (stdout, stderr) = text(&out);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("Surveillance Station 9.2.1-11575 on DS224+ (nas), 3 camera(s)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Trusted-device token: received"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Saved profile \"test\" (default)"),
        "{stdout}"
    );
    for secret in [SID, PASSWORD, "trusted-token"] {
        assert!(!stdout.contains(secret), "{stdout}");
        assert!(!stderr.contains(secret), "{stderr}");
    }

    let file = dir.path().join("surveillance-station").join("config.toml");
    let toml = std::fs::read_to_string(&file).unwrap();
    assert!(toml.contains("default_profile = \"test\""), "{toml}");
    assert!(toml.contains("[profiles.test]"), "{toml}");
    assert!(toml.contains("host = \"127.0.0.1\""), "{toml}");
    assert!(toml.contains("https = false"), "{toml}");
    assert!(toml.contains("verify_ssl = true"), "{toml}");
    assert!(toml.contains("device_id = \"trusted-token\""), "{toml}");
    assert!(toml.contains("username = \"alice\""), "{toml}");
    assert!(!toml.contains(PASSWORD), "{toml}");
    server.verify().await;
}
