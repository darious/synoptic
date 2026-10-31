//! `SYNO.SurveillanceStation.SnapShot`: take, list, download, delete.

use bytes::Bytes;
use serde_json::{Value, json};
use tokio::io::AsyncWrite;

use crate::client::{ApiRequest, Client, decode, list_field, total_field};
use crate::de;
use crate::error::Result;
use crate::models::{Page, Snapshot};

/// The snapshot API name.
pub const SNAPSHOT_API: &str = "SYNO.SurveillanceStation.SnapShot";

/// Filters for [`Client::list_snapshots`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotFilter {
    /// Sent as `camIdList`. Best effort only: at least one NAS ignores it
    /// and returns every camera's snapshots, so filter the result as well.
    pub camera_id: Option<i64>,
    /// Unix seconds (`from`).
    pub from_time: Option<i64>,
    /// Unix seconds (`to`).
    pub to_time: Option<i64>,
    /// Sent as `start` (the official client's name for the page offset).
    pub offset: u32,
    pub limit: u32,
}

impl Default for SnapshotFilter {
    fn default() -> Self {
        Self {
            camera_id: None,
            from_time: None,
            to_time: None,
            offset: 0,
            limit: 50,
        }
    }
}

impl Client {
    /// `SnapShot.TakeSnapshot` v1 with `blSave=true`: take a picture and
    /// store it in the NAS's snapshot database. Returns the new snapshot id.
    /// (`Camera.GetSnapshot` only returns a live JPEG and saves nothing.)
    pub async fn take_snapshot(&self, camera_id: i64, ds_id: i64) -> Result<i64> {
        let req = ApiRequest::new(SNAPSHOT_API, "TakeSnapshot", 1)
            .param("dsId", ds_id)
            .param("camId", camera_id)
            .param("blSave", true);
        let data = self.send(&req).await?;
        Ok(data.get("id").map(de::coerce_int).unwrap_or(0))
    }

    /// `SnapShot.List` v1. The list is under `data` (older: `snapshot`).
    pub async fn list_snapshots(&self, filter: &SnapshotFilter) -> Result<Page<Snapshot>> {
        let req = ApiRequest::new(SNAPSHOT_API, "List", 1)
            .param("start", filter.offset)
            .param("limit", filter.limit)
            .param_opt("camIdList", filter.camera_id)
            .param_opt("from", filter.from_time)
            .param_opt("to", filter.to_time);
        let data = self.send(&req).await?;
        let raw = list_field(&data, &["data", "snapshot"])
            .cloned()
            .unwrap_or(Value::Array(vec![]));
        let items: Vec<Snapshot> = decode(raw, "SnapShot.List")?;
        let total = total_field(&data, items.len());
        Ok(Page { items, total })
    }

    /// `SnapShot.Download` v1: the full-size image bytes.
    pub async fn snapshot_image(&self, snapshot_id: i64) -> Result<Bytes> {
        let req = ApiRequest::new(SNAPSHOT_API, "Download", 1).param("id", snapshot_id);
        self.download(&req).await
    }

    /// `SnapShot.Download` v1 streamed into `out`; returns the bytes written.
    pub async fn download_snapshot<W: AsyncWrite + Unpin + ?Sized>(
        &self,
        snapshot_id: i64,
        out: &mut W,
    ) -> Result<u64> {
        let req = ApiRequest::new(SNAPSHOT_API, "Download", 1).param("id", snapshot_id);
        self.download_to(&req, out).await
    }

    /// `SnapShot.Delete` v1 with `objList=[{"id":"<dsId>:<snapshotId>"}]`.
    /// (A plain `idList` is silently ignored by DSM and deletes everything.)
    pub async fn delete_snapshot(&self, snapshot_id: i64, ds_id: i64) -> Result<()> {
        let req = ApiRequest::new(SNAPSHOT_API, "Delete", 1).param(
            "objList",
            json!([{ "id": format!("{ds_id}:{snapshot_id}") }]),
        );
        self.send(&req).await.map(drop)
    }
}
