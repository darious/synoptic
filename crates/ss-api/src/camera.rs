//! `SYNO.SurveillanceStation.Camera`: list, info, live view paths, snapshot.

use bytes::Bytes;
use serde_json::Value;

use crate::client::{ApiRequest, Client, decode, list_field};
use crate::error::{Error, Result};
use crate::models::{Camera, LiveProtocol, LiveViewPath, StreamProfile};

/// The camera API name.
pub const CAMERA_API: &str = "SYNO.SurveillanceStation.Camera";

fn csv(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

impl Client {
    /// `Camera.List` v9 with `basic`, `streamInfo` and `ptz` set, as the
    /// reference asks for it.
    pub async fn list_cameras(&self) -> Result<Vec<Camera>> {
        let req = ApiRequest::new(CAMERA_API, "List", 9)
            .param("basic", true)
            .param("streamInfo", true)
            .param("ptz", true);
        let data = self.send(&req).await?;
        let cams = data.get("cameras").cloned().unwrap_or(Value::Array(vec![]));
        decode(cams, "Camera.List.cameras")
    }

    /// `Camera.GetInfo` v8 with `streamInfo=true` for `camera_ids`. Unlike
    /// `List`, this reports each camera's Live View profile
    /// ([`Camera::def_live_profile`]).
    pub async fn camera_info(&self, camera_ids: &[i64]) -> Result<Vec<Camera>> {
        let req = ApiRequest::new(CAMERA_API, "GetInfo", 8)
            .param("cameraIds", csv(camera_ids))
            .param("streamInfo", true);
        let data = self.send(&req).await?;
        let cams = data.get("cameras").cloned().unwrap_or(Value::Array(vec![]));
        decode(cams, "Camera.GetInfo.cameras")
    }

    /// The stream profile a camera's Live View setting selects
    /// (`defLiveProfile`), or [`StreamProfile::Balanced`] — Surveillance
    /// Station's own default — when the NAS does not report one. Request
    /// failures are returned, not swallowed; callers that want the
    /// reference's "any failure means Balanced" can `unwrap_or_default()`.
    pub async fn live_view_profile(&self, camera_id: i64) -> Result<StreamProfile> {
        let cams = self.camera_info(&[camera_id]).await?;
        Ok(cams
            .first()
            .and_then(|c| c.def_live_profile)
            .unwrap_or_default())
    }

    /// `Camera.GetLiveViewPath` v9 for `camera_ids`. The answer is either a
    /// bare list or an object with `pathInfos` (older: `cameras`); both are
    /// handled.
    pub async fn live_view_paths(&self, camera_ids: &[i64]) -> Result<Vec<LiveViewPath>> {
        let req =
            ApiRequest::new(CAMERA_API, "GetLiveViewPath", 9).param("idList", csv(camera_ids));
        let data = self.send(&req).await?;
        let list = match &data {
            Value::Array(_) => data.clone(),
            Value::Object(_) => list_field(&data, &["pathInfos", "cameras"])
                .cloned()
                .unwrap_or(Value::Array(vec![])),
            _ => Value::Array(vec![]),
        };
        decode(list, "Camera.GetLiveViewPath")
    }

    /// A playable URL for `camera_id` over `protocol`, from the first
    /// `GetLiveViewPath` entry. A relative MJPEG path is prefixed with the
    /// base URL. Errors with [`Error::InvalidResponse`] when the camera has
    /// no path at all or does not offer `protocol`.
    pub async fn live_view_url(&self, camera_id: i64, protocol: LiveProtocol) -> Result<String> {
        let paths = self.live_view_paths(&[camera_id]).await?;
        let Some(info) = paths.first() else {
            return Err(Error::InvalidResponse(format!(
                "no live view path for camera {camera_id}"
            )));
        };
        info.url(protocol, self.base_url()).ok_or_else(|| {
            Error::InvalidResponse(format!(
                "protocol {protocol:?} not available for camera {camera_id}"
            ))
        })
    }

    /// `Camera.GetSnapshot` v3: the camera's current picture as a JPEG,
    /// saved nowhere on the NAS.
    pub async fn camera_snapshot(&self, camera_id: i64) -> Result<Bytes> {
        let req = ApiRequest::new(CAMERA_API, "GetSnapshot", 3).param("cameraId", camera_id);
        self.download(&req).await
    }
}
