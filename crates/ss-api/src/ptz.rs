//! `SYNO.SurveillanceStation.PTZ`: moves, zoom, focus, presets, patrols.

use crate::client::{ApiRequest, Client, decode};
use crate::error::Result;
use crate::models::{PtzPatrol, PtzPreset};

/// The PTZ API name.
pub const PTZ_API: &str = "SYNO.SurveillanceStation.PTZ";

/// `PTZ.Move` directions. Continuous moves are started and stopped with
/// separate calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PtzDirection {
    UpStart,
    UpStop,
    DownStart,
    DownStop,
    LeftStart,
    LeftStop,
    RightStart,
    RightStop,
    Home,
}

impl PtzDirection {
    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UpStart => "upStart",
            Self::UpStop => "upStop",
            Self::DownStart => "downStart",
            Self::DownStop => "downStop",
            Self::LeftStart => "leftStart",
            Self::LeftStop => "leftStop",
            Self::RightStart => "rightStart",
            Self::RightStop => "rightStop",
            Self::Home => "home",
        }
    }
}

/// `PTZ.Zoom` controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ZoomControl {
    InStart,
    InStop,
    OutStart,
    OutStop,
}

impl ZoomControl {
    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InStart => "inStart",
            Self::InStop => "inStop",
            Self::OutStart => "outStart",
            Self::OutStop => "outStop",
        }
    }
}

/// `PTZ.Focus` controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FocusControl {
    InStart,
    InStop,
    OutStart,
    OutStop,
}

impl FocusControl {
    fn parts(self) -> (&'static str, &'static str) {
        match self {
            Self::InStart => ("in", "Start"),
            Self::InStop => ("in", "Stop"),
            Self::OutStart => ("out", "Start"),
            Self::OutStop => ("out", "Stop"),
        }
    }
}

impl Client {
    /// `PTZ.Move` v2.
    pub async fn ptz_move(&self, camera_id: i64, direction: PtzDirection) -> Result<()> {
        let req = ApiRequest::new(PTZ_API, "Move", 2)
            .param("cameraId", camera_id)
            .param("direction", direction.as_str());
        self.send(&req).await.map(drop)
    }

    /// `PTZ.Zoom` v2.
    pub async fn ptz_zoom(&self, camera_id: i64, control: ZoomControl) -> Result<()> {
        let req = ApiRequest::new(PTZ_API, "Zoom", 2)
            .param("cameraId", camera_id)
            .param("control", control.as_str());
        self.send(&req).await.map(drop)
    }

    /// `PTZ.Focus` v6, with the two fixed parameters DSM's own web UI sends.
    pub async fn ptz_focus(&self, camera_id: i64, control: FocusControl) -> Result<()> {
        let (ctrl, move_type) = control.parts();
        let req = ApiRequest::new(PTZ_API, "Focus", 6)
            .param("cameraId", camera_id)
            .param("moveType", move_type)
            .param("control", ctrl)
            .param("direction", 1)
            .param("profileType", 0);
        self.send(&req).await.map(drop)
    }

    /// `PTZ.ListPreset` v2.
    pub async fn ptz_presets(&self, camera_id: i64) -> Result<Vec<PtzPreset>> {
        let req = ApiRequest::new(PTZ_API, "ListPreset", 2).param("cameraId", camera_id);
        let data = self.send(&req).await?;
        match data.get("presets") {
            Some(v) => decode(v.clone(), "PTZ.ListPreset"),
            None => Ok(Vec::new()),
        }
    }

    /// `PTZ.GoPreset` v2.
    pub async fn ptz_go_preset(&self, camera_id: i64, preset_id: i64) -> Result<()> {
        let req = ApiRequest::new(PTZ_API, "GoPreset", 2)
            .param("cameraId", camera_id)
            .param("presetId", preset_id);
        self.send(&req).await.map(drop)
    }

    /// `PTZ.ListPatrol` v2.
    pub async fn ptz_patrols(&self, camera_id: i64) -> Result<Vec<PtzPatrol>> {
        let req = ApiRequest::new(PTZ_API, "ListPatrol", 2).param("cameraId", camera_id);
        let data = self.send(&req).await?;
        match data.get("patrols") {
            Some(v) => decode(v.clone(), "PTZ.ListPatrol"),
            None => Ok(Vec::new()),
        }
    }

    /// `PTZ.RunPatrol` v2. The NAS drives the route; there is no stop call.
    pub async fn ptz_run_patrol(&self, camera_id: i64, patrol_id: i64) -> Result<()> {
        let req = ApiRequest::new(PTZ_API, "RunPatrol", 2)
            .param("cameraId", camera_id)
            .param("patrolId", patrol_id);
        self.send(&req).await.map(drop)
    }
}
