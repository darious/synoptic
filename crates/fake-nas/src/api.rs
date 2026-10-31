//! The DSM Web API: `/webapi/<cgi>` with `api`/`method`/`version` in the
//! query string or form body, answered with the
//! `{"success":true,"data":…}` / `{"success":false,"error":{"code":N}}`
//! envelope `ss_api` decodes.
//!
//! Shapes are the ones our client code reads (see `ss-api`'s models and
//! wiremock fixtures and the raw `ApiRequest`s in the UI), not a full copy
//! of Synology's.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use bytes::Bytes;
use serde_json::{Value, json};

use crate::Config;
use crate::media::Media;
use crate::rng::Rng;
use crate::world::{self, CameraDef, World};

/// Synology common error codes used here.
pub const ERR_UNKNOWN: i64 = 100;
pub const ERR_INVALID_PARAM: i64 = 101;
pub const ERR_NO_API: i64 = 102;
pub const ERR_NO_METHOD: i64 = 103;
pub const ERR_NO_SESSION: i64 = 119;
/// `SYNO.API.Auth` codes.
pub const AUTH_BAD_CREDENTIALS: i64 = 400;
pub const AUTH_OTP_REQUIRED: i64 = 403;
pub const AUTH_OTP_INVALID: i64 = 404;
/// Surveillance Station codes.
pub const SS_EXEC_FAILED: i64 = 400;
pub const SS_PARAM_INVALID: i64 = 401;
pub const SS_CAMERA_DISABLED: i64 = 402;

const MAX_BODY: usize = 1 << 20;

/// Everything the handlers share.
pub struct App {
    pub cfg: Config,
    pub media: Arc<Media>,
    pub world: World,
    sessions: Mutex<Sessions>,
}

#[derive(Default)]
struct Sessions {
    sids: HashSet<String>,
    dids: HashSet<String>,
    serial: u64,
}

impl Sessions {
    fn new_token(&mut self) -> String {
        self.serial += 1;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut rng = Rng::from_parts(&[nanos, self.serial, 0x5E55]);
        format!("{:016x}{:016x}", rng.next_u64(), rng.next_u64())
    }
}

/// Request parameters from the query string and/or form body.
#[derive(Clone, Debug, Default)]
pub struct Params(HashMap<String, String>);

impl Params {
    pub fn parse_into(&mut self, encoded: &[u8]) {
        for (k, v) in url::form_urlencoded::parse(encoded) {
            self.0.insert(k.into_owned(), v.into_owned());
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        let s = self.get(key)?.trim();
        s.parse::<i64>()
            .ok()
            .or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
    }

    pub fn boolean(&self, key: &str) -> Option<bool> {
        Some(matches!(
            self.get(key)?.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ))
    }

    /// `1,2,3`, `[1,2,3]` or a single number.
    pub fn ids(&self, key: &str) -> Vec<i64> {
        self.get(key)
            .unwrap_or("")
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .filter_map(|p| p.trim().trim_matches('"').parse::<i64>().ok())
            .collect()
    }

    pub fn json(&self, key: &str) -> Option<Value> {
        serde_json::from_str(self.get(key)?).ok()
    }
}

/// What a handler produces.
pub enum Reply {
    Json(Value),
    Error(i64),
    File(&'static str, Bytes),
}

impl IntoResponse for Reply {
    fn into_response(self) -> Response {
        match self {
            Reply::Json(data) => Json(json!({"success": true, "data": data})).into_response(),
            Reply::Error(code) => {
                Json(json!({"success": false, "error": {"code": code}})).into_response()
            }
            Reply::File(content_type, bytes) => {
                ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
            }
        }
    }
}

fn ok() -> Reply {
    Reply::Json(json!({}))
}

fn int_of(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// The `SYNO.API.Info` table: every API this server answers.
pub fn api_info_table() -> Value {
    let entry = |path: &str, max: u32| json!({"path": path, "minVersion": 1, "maxVersion": max});
    json!({
        "SYNO.API.Info": entry("query.cgi", 1),
        "SYNO.API.Auth": entry("auth.cgi", 7),
        "SYNO.SurveillanceStation.Info": entry("entry.cgi", 8),
        "SYNO.SurveillanceStation.Camera": entry("entry.cgi", 9),
        "SYNO.SurveillanceStation.PTZ": entry("entry.cgi", 6),
        "SYNO.SurveillanceStation.Recording": entry("entry.cgi", 6),
        "SYNO.SurveillanceStation.RecordingPicker": entry("entry.cgi", 1),
        "SYNO.SurveillanceStation.EventCenter.Event": entry("entry.cgi", 1),
        "SYNO.SurveillanceStation.Notification": entry("entry.cgi", 1),
        "SYNO.SurveillanceStation.SnapShot": entry("entry.cgi", 1),
        "SYNO.SurveillanceStation.License": entry("entry.cgi", 1),
        "SYNO.SurveillanceStation.HomeMode": entry("entry.cgi", 1),
        "SYNO.SurveillanceStation.TimeLapse": entry("entry.cgi", 1),
        "SYNO.SurveillanceStation.TimeLapse.Recording": entry("entry.cgi", 1),
        "SYNO.SurveillanceStation.Stream": entry("entry.cgi", 1),
    })
}

impl App {
    pub fn new(cfg: Config) -> Result<App, crate::media::MediaError> {
        let media = Media::load(&cfg.fixtures)?;
        tracing::info!(?media, "fixtures loaded");
        Ok(App {
            world: World::new(cfg.seed),
            cfg,
            media: Arc::new(media),
            sessions: Mutex::new(Sessions::default()),
        })
    }

    fn sessions(&self) -> MutexGuard<'_, Sessions> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `sid` names a live session.
    pub fn is_session(&self, sid: &str) -> bool {
        self.sessions().sids.contains(sid)
    }

    /// Route one call.
    pub fn dispatch(&self, p: &Params) -> Reply {
        let api = p.get("api").unwrap_or("");
        let method = p.get("method").unwrap_or("");
        let version = p.int("version").unwrap_or(1);
        tracing::debug!(api, method, version, "api request");
        match api {
            "SYNO.API.Info" => match method {
                "Query" => Reply::Json(api_info_table()),
                _ => Reply::Error(ERR_NO_METHOD),
            },
            "SYNO.API.Auth" => self.auth(method, p),
            _ if api.starts_with("SYNO.SurveillanceStation.") => {
                if !p.get("_sid").is_some_and(|s| self.is_session(s)) {
                    tracing::info!(api, method, "rejected: no session");
                    return Reply::Error(ERR_NO_SESSION);
                }
                let reply = self.surveillance(api, method, version, p);
                if let Reply::Error(code) = &reply {
                    tracing::info!(api, method, code, "api error");
                }
                reply
            }
            _ => Reply::Error(ERR_NO_API),
        }
    }

    fn auth(&self, method: &str, p: &Params) -> Reply {
        match method {
            "Login" => {
                let account = p.get("account").unwrap_or("");
                let passwd = p.get("passwd").unwrap_or("");
                if account != self.cfg.user || passwd != self.cfg.password {
                    tracing::info!(account, "login rejected: bad credentials");
                    return Reply::Error(AUTH_BAD_CREDENTIALS);
                }
                let mut sessions = self.sessions();
                if let Some(expected) = &self.cfg.otp {
                    let trusted = p
                        .get("device_id")
                        .is_some_and(|d| sessions.dids.contains(d));
                    if !trusted {
                        match p.get("otp_code").filter(|c| !c.is_empty()) {
                            None => {
                                tracing::info!(account, "login needs an OTP code");
                                return Reply::Error(AUTH_OTP_REQUIRED);
                            }
                            Some(code) if code != expected => {
                                tracing::info!(account, "login rejected: wrong OTP code");
                                return Reply::Error(AUTH_OTP_INVALID);
                            }
                            Some(_) => {}
                        }
                    }
                }
                let sid = sessions.new_token();
                sessions.sids.insert(sid.clone());
                let mut data = json!({"sid": sid, "is_portal_port": false});
                if p.boolean("enable_device_token") == Some(true) {
                    let did = sessions.new_token();
                    sessions.dids.insert(did.clone());
                    data["did"] = json!(did);
                }
                tracing::info!(
                    account,
                    device_token = data.get("did").is_some(),
                    "login ok"
                );
                Reply::Json(data)
            }
            "Logout" => {
                if let Some(sid) = p.get("_sid") {
                    self.sessions().sids.remove(sid);
                }
                tracing::info!("logout");
                ok()
            }
            _ => Reply::Error(ERR_NO_METHOD),
        }
    }

    fn surveillance(&self, api: &str, method: &str, version: i64, p: &Params) -> Reply {
        let api = api.trim_start_matches("SYNO.SurveillanceStation.");
        match (api, method) {
            ("Info", "GetInfo") => self.info(),
            ("Camera", "List") => Reply::Json(json!({
                "cameras": self.world.cameras().iter().map(|c| self.camera_json(c, false)).collect::<Vec<_>>(),
                "total": self.world.cameras().len(),
            })),
            ("Camera", "GetInfo") => {
                let ids = p.ids("cameraIds");
                let cams: Vec<Value> = self
                    .world
                    .cameras()
                    .iter()
                    .filter(|c| ids.is_empty() || ids.contains(&c.id))
                    .map(|c| self.camera_json(c, true))
                    .collect();
                Reply::Json(json!({"cameras": cams}))
            }
            ("Camera", "GetLiveViewPath") => {
                let ids = p.ids("idList");
                let paths: Vec<Value> = ids
                    .iter()
                    .filter_map(|id| self.world.camera(*id))
                    .map(|c| json!({
                        "id": c.id,
                        "rtspPath": format!("rtsp://127.0.0.1:554/Sms={}.unicast", c.id),
                        "rtspOverHttpPath": format!("http://127.0.0.1:{}/webapi/entry.cgi?api=SYNO.SurveillanceStation.Stream&method=LiveStream&cameraId={}", self.cfg.advertised_port, c.id),
                        "mjpegHttpPath": format!("/webapi/entry.cgi?api=SYNO.SurveillanceStation.Stream&method=LiveStream&version=1&cameraId={}&format=mjpeg", c.id),
                        "multicstPath": "",
                    }))
                    .collect();
                Reply::Json(Value::Array(paths))
            }
            ("Camera", "GetSnapshot") => {
                let Some(cam) = p.int("cameraId").and_then(|id| self.world.camera(id)) else {
                    return Reply::Error(SS_PARAM_INVALID);
                };
                if !cam.enabled() {
                    return Reply::Error(SS_CAMERA_DISABLED);
                }
                Reply::File("image/jpeg", self.media.jpeg.clone())
            }
            ("Camera", _) => Reply::Error(ERR_NO_METHOD),

            ("Recording", "List") => self.recording_list(p),
            ("Recording", "Download") => {
                let id = p.int("id").or_else(|| p.int("eventId"));
                if id.is_none() {
                    return Reply::Error(SS_PARAM_INVALID);
                }
                tracing::info!(id, version, "recording download");
                Reply::File("video/mp4", self.media.mp4.clone())
            }
            ("Recording", "GetThumbnail") => {
                let items = match p.json("eventInfo") {
                    Some(Value::Array(items)) => items,
                    Some(other) => vec![other],
                    None => return Reply::Error(SS_PARAM_INVALID),
                };
                let thumbs: Vec<Value> = items
                    .iter()
                    .map(|e| {
                        json!({
                            "cameraId": int_of(e.get("cameraId")),
                            "targetTime": int_of(e.get("targetTime")),
                            "thumbnail": self.media.thumbnail_b64,
                        })
                    })
                    .collect();
                Reply::Json(Value::Array(thumbs))
            }
            ("Recording", "Keepalive") => ok(),
            ("Recording", _) => Reply::Error(ERR_NO_METHOD),
            ("Stream", "EventStream") => Reply::File("video/mp4", self.media.mp4.clone()),
            ("Stream", _) => Reply::Error(ERR_NO_METHOD),

            ("RecordingPicker", "EnumInterval") => self.enum_interval(p),
            ("RecordingPicker", _) => Reply::Error(ERR_NO_METHOD),

            ("EventCenter.Event", "Capability") => Reply::Json(json!({
                "event_types": "1,2,3,10,11,12,18,19",
                "object_types": "1,2,3,4",
            })),
            ("EventCenter.Event", "List") => self.event_center_list(p),
            ("EventCenter.Event", _) => Reply::Error(ERR_NO_METHOD),

            ("Notification", "List") => {
                let offset = p.int("offset").unwrap_or(0).max(0) as usize;
                let limit = p.int("limit").unwrap_or(50).max(0) as usize;
                let alerts = self.world.alerts();
                let total = alerts.len();
                let page: Vec<Value> = alerts
                    .iter()
                    .rev()
                    .skip(offset)
                    .take(limit)
                    .map(|a| {
                        json!({
                            "id": a.id,
                            "cameraId": a.camera_id,
                            "cameraName": self.world.camera_name(a.camera_id),
                            "alertType": a.alert_type,
                            "timestamp": a.timestamp,
                            "isRead": a.is_read,
                        })
                    })
                    .collect();
                Reply::Json(json!({"notifications": page, "total": total}))
            }
            ("Notification", "GetUnreadCount") => {
                Reply::Json(json!({"unread": self.world.unread_alerts()}))
            }
            ("Notification", "SetRead") => {
                self.world.mark_alerts_read(&p.ids("idList"));
                ok()
            }
            ("Notification", _) => Reply::Error(ERR_NO_METHOD),

            ("SnapShot", "List") => self.snapshot_list(p),
            ("SnapShot", "Download") => match p.int("id").and_then(|id| self.world.snapshot(id)) {
                Some(_) => Reply::File("image/jpeg", self.media.jpeg.clone()),
                None => Reply::Error(SS_EXEC_FAILED),
            },
            ("SnapShot", "Delete") => {
                let ids: Vec<i64> = match p.json("objList") {
                    Some(Value::Array(items)) => items
                        .iter()
                        .filter_map(|o| o.get("id"))
                        .filter_map(|id| match id {
                            Value::String(s) => s.rsplit(':').next()?.parse().ok(),
                            other => Some(int_of(Some(other))),
                        })
                        .collect(),
                    _ => p.ids("idList"),
                };
                let n = self.world.delete_snapshots(&ids);
                tracing::info!(deleted = n, "snapshots deleted");
                ok()
            }
            ("SnapShot", "TakeSnapshot") => {
                let Some(cam) = p.int("camId").and_then(|id| self.world.camera(id)) else {
                    return Reply::Error(SS_PARAM_INVALID);
                };
                if !cam.enabled() {
                    return Reply::Error(SS_CAMERA_DISABLED);
                }
                if p.boolean("blSave") == Some(true) {
                    let id = self.world.take_snapshot(cam.id);
                    Reply::Json(json!({"id": id, "camName": cam.name}))
                } else {
                    Reply::File("image/jpeg", self.media.jpeg.clone())
                }
            }
            ("SnapShot", _) => Reply::Error(ERR_NO_METHOD),

            ("PTZ", method) => self.ptz(method, p),

            ("License", "Load") => {
                let licenses = self.world.licenses();
                let quota: i64 = licenses.iter().map(|l| l.quota).sum();
                Reply::Json(json!({
                    "key_max": world::MAX_CAMERAS,
                    "key_total": world::FREE_LICENSES + quota,
                    "key_used": self.world.enabled_camera_count(),
                    "license": licenses.iter().map(|l| json!({
                        "id": l.id, "key": l.key, "quota": l.quota, "expired_date": 0,
                        "isExpired": false, "isMigrated": false, "ownerDsId": 0,
                    })).collect::<Vec<_>>(),
                }))
            }
            ("License", "AddKey") => {
                let raw = p.get("licenseList").unwrap_or("");
                let keys: Vec<&str> = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|k| !k.is_empty())
                    .collect();
                if keys.is_empty() {
                    return Reply::Error(SS_PARAM_INVALID);
                }
                self.world.add_licenses(&keys);
                tracing::info!(count = keys.len(), "licences added");
                ok()
            }
            ("License", "DeleteKey") => {
                self.world.delete_licenses(&p.ids("lic_list"));
                ok()
            }
            ("License", _) => Reply::Error(ERR_NO_METHOD),

            ("HomeMode", "GetInfo") => Reply::Json(json!({
                "on": self.world.home_mode(),
                "notify_on": true, "onetime_disable_on": false, "onetime_enable_on": false,
            })),
            ("HomeMode", "Switch") => {
                let Some(on) = p.boolean("on") else {
                    return Reply::Error(SS_PARAM_INVALID);
                };
                self.world.set_home_mode(on);
                tracing::info!(on, "home mode switched");
                ok()
            }
            ("HomeMode", _) => Reply::Error(ERR_NO_METHOD),

            ("TimeLapse", "ListTask") => Reply::Json(json!({
                "task": world::TIMELAPSE_TASKS.iter().map(|t| json!({
                    "id": t.id, "name": t.name, "cameraId": t.camera_id,
                    "cameraName": self.world.camera_name(t.camera_id),
                    "enabled": true, "status": 2,
                })).collect::<Vec<_>>(),
                "total": world::TIMELAPSE_TASKS.len(),
            })),
            ("TimeLapse", _) => Reply::Error(ERR_NO_METHOD),
            ("TimeLapse.Recording", "List") => {
                let range = match (p.int("fromTime"), p.int("toTime")) {
                    (Some(f), Some(t)) => Some((f, t)),
                    _ => None,
                };
                let task = p.int("lapseId").filter(|id| *id >= 0);
                let recs: Vec<Value> = self
                    .world
                    .timelapse_recordings(range)
                    .iter()
                    .filter(|r| task.is_none_or(|t| t == r.task_id))
                    .map(|r| json!({
                        "id": r.id,
                        "cameraId": r.camera_id,
                        "camera_name": self.world.camera_name(r.camera_id),
                        "startTime": r.start,
                        "stopTime": r.stop,
                        "event_size_bytes": self.media.mp4.len() as i64 * 20,
                        "mountId": 0,
                        "archId": 0,
                        "status_flags": if r.locked { 4 } else { 0 },
                        "recording": r.recording,
                        "path": format!("/volume1/surveillance/timelapse/{}/{}.mp4", r.task_id, r.id),
                    }))
                    .collect();
                Reply::Json(json!({"events": recs, "total": recs.len()}))
            }
            ("TimeLapse.Recording", "Lock") => {
                self.world.set_timelapse_locked(&p.ids("idList"), true);
                ok()
            }
            ("TimeLapse.Recording", "Unlock") => {
                self.world.set_timelapse_locked(&p.ids("idList"), false);
                ok()
            }
            ("TimeLapse.Recording", "Delete") => {
                self.world.delete_timelapse(&p.ids("idList"));
                ok()
            }
            ("TimeLapse.Recording", "Download") => Reply::File("video/mp4", self.media.mp4.clone()),
            ("TimeLapse.Recording", _) => Reply::Error(ERR_NO_METHOD),

            _ => Reply::Error(ERR_NO_API),
        }
    }

    fn info(&self) -> Reply {
        let (major, minor, small, build) = world::SS_VERSION;
        Reply::Json(json!({
            "version": {
                "major": major.to_string(), "minor": minor.to_string(),
                "small": small.to_string(), "build": build.to_string(),
            },
            "serial": world::SERIAL,
            "DSModelName": world::DS_MODEL,
            "hostname": world::HOSTNAME,
            "cameraNumber": self.world.cameras().len(),
            "liscenseNumber": world::FREE_LICENSES + self.world.licenses().iter().map(|l| l.quota).sum::<i64>(),
            "maxCameraSupport": world::MAX_CAMERAS,
            "isLicenseEnough": true,
            "customizedPortHttp": 9900,
            "customizedPortHttps": 9901,
            "path": "/webman/3rdparty/SurveillanceStation/",
            "timezone": "Europe/London",
            "timezoneTZDB": "Europe/London",
            "uid": 1024,
            "userPriv": 1,
            "strInetPort": "5000",
            "enableVideoRelay": false,
        }))
    }

    fn camera_json(&self, c: &CameraDef, with_profile: bool) -> Value {
        let mut v = json!({
            "id": c.id,
            "name": c.name,
            "newName": c.name,
            "vendor": c.vendor,
            "model": c.model,
            "ip": c.ip,
            "port": 80,
            "channel": "1",
            "status": c.status,
            "enabled": c.enabled(),
            "recStatus": if c.enabled() { 1 } else { 0 },
            "deviceType": 1,
            "ptzDirection": if c.ptz { 1 } else { 0 },
            "ptzCap": if c.ptz { 15 } else { 0 },
            "audioCodec": if c.audio { 4 } else { 0 },
            "audioType": if c.audio { 4 } else { 0 },
            "audioOut": c.ptz,
            "videoCodec": c.video_codec,
            "resolution": c.resolution,
            "fps": 15,
            "ownerDsId": 0,
        });
        if with_profile {
            v["defLiveProfile"] = json!(c.def_live_profile);
            v["stmFisheyeType"] = json!(0);
        }
        v
    }

    fn recording_list(&self, p: &Params) -> Reply {
        let now = world::now_secs();
        let offset = p.int("offset").unwrap_or(0).max(0) as usize;
        let limit = p.int("limit").unwrap_or(50).max(0) as usize;
        let from = p.int("fromTime").unwrap_or(now - world::HORIZON_SECS);
        let to = p.int("toTime").unwrap_or(now);
        let ids = p.ids("cameraIds");
        let cameras: Vec<i64> = if ids.is_empty() {
            self.world.cameras().iter().map(|c| c.id).collect()
        } else {
            ids
        };
        let mut recs: Vec<_> = cameras
            .iter()
            .flat_map(|c| self.world.recordings(*c, from, to, now))
            .collect();
        recs.sort_by(|a, b| b.start.cmp(&a.start).then(a.camera_id.cmp(&b.camera_id)));
        let total = recs.len();
        let page: Vec<Value> = recs
            .iter()
            .skip(offset)
            .take(limit)
            .map(|r| json!({
                "id": r.id,
                "cameraId": r.camera_id,
                "camera_name": self.world.camera_name(r.camera_id),
                "cameraName": self.world.camera_name(r.camera_id),
                "startTime": r.start,
                "stopTime": r.stop,
                "fileSize": r.file_size(),
                "type": 0,
                "mountId": 0,
                "archId": 0,
                "defaultLabel": r.label,
                "recording": r.recording,
                "status": if r.recording { 1 } else { 0 },
                "filePath": format!("/volume1/surveillance/{}/{}.mp4", self.world.camera_name(r.camera_id), r.id),
            }))
            .collect();
        Reply::Json(json!({"events": page, "total": total, "offset": offset}))
    }

    fn enum_interval(&self, p: &Params) -> Reply {
        let now = world::now_secs();
        let (Some(from), Some(to)) = (p.int("from"), p.int("to")) else {
            return Reply::Error(SS_PARAM_INVALID);
        };
        let interval = p.int("interval").unwrap_or(5).max(1);
        let cam_ids: Vec<i64> = match p.json("content") {
            Some(Value::Array(groups)) => groups
                .iter()
                .filter_map(|g| g.get("camList"))
                .filter_map(Value::as_array)
                .flatten()
                .map(|v| int_of(Some(v)))
                .collect(),
            _ => return Reply::Error(SS_PARAM_INVALID),
        };
        let cams: Vec<Value> = cam_ids
            .iter()
            .filter_map(|id| self.world.camera(*id))
            .map(|c| {
                let recs: Vec<Value> = self
                    .world
                    .recordings(c.id, from, to, now)
                    .iter()
                    .map(|r| json!({"id": r.id, "start": r.start, "stop": r.stop}))
                    .collect();
                json!({
                    "camera_id": c.id,
                    "dsId": 0,
                    "mountId": 0,
                    "archId": 0,
                    "event": recs,
                    "event_map": self.world.event_map(c.id, from, to, interval, now),
                })
            })
            .collect();
        Reply::Json(json!({"cameras": [cams]}))
    }

    fn event_center_list(&self, p: &Params) -> Reply {
        let now = world::now_secs();
        let ids = p.ids("camera_ids");
        let from = p.int("start_time").unwrap_or(now - world::HORIZON_SECS);
        let to = p.int("end_time").unwrap_or(now);
        let limit = p.int("limit").unwrap_or(1000).max(0) as usize;
        let offset = p
            .json("start_time_offset")
            .and_then(|v| v.get("0").cloned())
            .map(|o| (int_of(o.get("start_time")), int_of(o.get("id"))))
            .filter(|(s, i)| *s > 0 || *i > 0);
        let mut events: Vec<_> = ids
            .iter()
            .flat_map(|c| self.world.events(*c, from, to, now))
            .collect();
        events.sort_by_key(|e| std::cmp::Reverse((e.start, e.id)));
        if let Some(cursor) = offset {
            events.retain(|e| (e.start, e.id) < cursor);
        }
        events.truncate(limit);
        let page: Vec<Value> = events
            .iter()
            .map(|e| {
                let (event_type, object_type) = e.kind.event_center_type();
                let duration = if e.start + e.duration > now {
                    0
                } else {
                    e.duration
                };
                json!({
                    "id": e.id,
                    "ds_id": 0,
                    "camera_id": e.camera_id,
                    "camera_name": self.world.camera_name(e.camera_id),
                    "start_time": e.start,
                    "duration": duration,
                    "event_type": event_type,
                    "object_type": object_type,
                    "recording_type": e.kind.rec_type(),
                })
            })
            .collect();
        Reply::Json(json!({"0": page}))
    }

    fn snapshot_list(&self, p: &Params) -> Reply {
        let start = p.int("start").unwrap_or(0).max(0) as usize;
        let limit = p.int("limit").unwrap_or(50).max(0) as usize;
        let cams = p.ids("camIdList");
        let from = p.int("from");
        let to = p.int("to");
        let snaps: Vec<_> = self
            .world
            .snapshots()
            .into_iter()
            .filter(|s| cams.is_empty() || cams.contains(&s.camera_id))
            .filter(|s| from.is_none_or(|f| s.created >= f) && to.is_none_or(|t| s.created <= t))
            .collect();
        let total = snaps.len();
        let page: Vec<Value> = snaps
            .iter()
            .rev()
            .skip(start)
            .take(limit)
            .map(|s| {
                json!({
                    "id": s.id,
                    "camId": s.camera_id,
                    "camName": self.world.camera_name(s.camera_id),
                    "createdTm": s.created,
                    "displayTm": s.created,
                    "imageData": self.media.thumbnail_b64,
                    "size": self.media.jpeg.len(),
                })
            })
            .collect();
        Reply::Json(json!({"data": page, "total": total}))
    }

    fn ptz(&self, method: &str, p: &Params) -> Reply {
        let Some(cam) = p.int("cameraId").and_then(|id| self.world.camera(id)) else {
            return Reply::Error(SS_PARAM_INVALID);
        };
        if !cam.ptz {
            return Reply::Error(SS_EXEC_FAILED);
        }
        match method {
            "Move" | "Zoom" | "Focus" | "GoPreset" | "RunPatrol" | "Home" | "AutoFocus" => {
                tracing::info!(
                    camera = cam.name,
                    method,
                    direction = p.get("direction"),
                    control = p.get("control"),
                    preset = p.get("presetId"),
                    patrol = p.get("patrolId"),
                    "ptz"
                );
                ok()
            }
            "ListPreset" => Reply::Json(json!({
                "presets": [
                    {"id": 1, "name": "Home", "position": 0},
                    {"id": 2, "name": "Gate", "position": 1},
                    {"id": 3, "name": "Porch", "position": 2},
                ],
                "total": 3,
            })),
            "ListPatrol" => Reply::Json(json!({
                "patrols": [{"id": 1, "name": "Perimeter"}],
                "total": 1,
            })),
            _ => Reply::Error(ERR_NO_METHOD),
        }
    }
}

/// `GET`/`POST /webapi/<cgi>`.
pub async fn webapi(
    State(app): State<Arc<App>>,
    Path(_cgi): Path<String>,
    req: Request,
) -> Response {
    let query = req.uri().query().unwrap_or("").to_owned();
    let is_post = req.method() == Method::POST;
    let is_form = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/x-www-form-urlencoded"));
    let mut params = Params::default();
    params.parse_into(query.as_bytes());
    if is_post {
        match axum::body::to_bytes(req.into_body(), MAX_BODY).await {
            Ok(body) if is_form || !body.is_empty() => params.parse_into(&body),
            Ok(_) => {}
            Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response(),
        }
    }
    app.dispatch(&params).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_parse_forms_and_lists() {
        let mut p = Params::default();
        p.parse_into(
            b"api=SYNO.API.Auth&enable_device_token=yes&ids=1%2C2%2C3&arr=%5B4%2C5%5D&n=7",
        );
        assert_eq!(p.get("api"), Some("SYNO.API.Auth"));
        assert_eq!(p.boolean("enable_device_token"), Some(true));
        assert_eq!(p.ids("ids"), vec![1, 2, 3]);
        assert_eq!(p.ids("arr"), vec![4, 5]);
        assert_eq!(p.ids("n"), vec![7]);
        assert_eq!(p.int("n"), Some(7));
        assert_eq!(p.int("missing"), None);
    }

    #[test]
    fn api_table_lists_every_served_api() {
        let t = api_info_table();
        for api in [
            "SYNO.API.Auth",
            "SYNO.SurveillanceStation.Camera",
            "SYNO.SurveillanceStation.RecordingPicker",
            "SYNO.SurveillanceStation.EventCenter.Event",
        ] {
            assert!(t.get(api).is_some(), "{api}");
        }
        assert_eq!(t["SYNO.API.Auth"]["path"], "auth.cgi");
    }
}
