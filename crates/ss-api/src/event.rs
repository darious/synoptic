//! Events: `EventCenter.Event` (SS 9.x typed event log),
//! `RecordingPicker.EnumInterval` (recording presence + legacy event map) and
//! `Notification` alerts.

use std::time::Duration;

use serde_json::{Value, json};

use crate::client::{ApiRequest, Client, decode, list_field, total_field};
use crate::error::Result;
use crate::models::{Alert, EventCenterCapability, EventCenterEvent, IntervalCamera, Page};

/// DSM's own Event Center API; absent on older Surveillance Station
/// releases (check with [`Client::has_api`]).
pub const EVENT_CENTER_API: &str = "SYNO.SurveillanceStation.EventCenter.Event";
/// The recording-picker API behind the Monitor Center timeline.
pub const RECORDING_PICKER_API: &str = "SYNO.SurveillanceStation.RecordingPicker";
/// The notification (alerts) API.
pub const NOTIFICATION_API: &str = "SYNO.SurveillanceStation.Notification";
/// `EnumInterval`'s `interval`: the bucket size in seconds of the returned
/// `event_map`. 5 is what DSM's own web UI asks for.
pub const ENUM_INTERVAL_SEC: u32 = 5;
/// `EnumInterval` over a wide range with many cameras has been seen to take
/// well over the default timeout.
pub const ENUM_INTERVAL_TIMEOUT: Duration = Duration::from_secs(120);

const EVENT_CENTER_PAGE_SIZE: u32 = 1000;
const LOCAL_DS_ID: &str = "0";
const SQL_ORDER_DESC: u8 = 1;

fn csv(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

impl Client {
    /// `EventCenter.Event.Capability`: which event and object types this
    /// NAS can report.
    pub async fn event_center_capability(&self) -> Result<EventCenterCapability> {
        let req = ApiRequest::new(EVENT_CENTER_API, "Capability", 1);
        self.send_as(&req).await
    }

    /// Every `EventCenter.Event.List` entry for `camera_ids` within
    /// `[from_time, to_time]` (unix seconds), newest first, paged through
    /// until an empty page or a page that repeats the last offset.
    pub async fn list_event_center_events(
        &self,
        camera_ids: &[i64],
        from_time: i64,
        to_time: i64,
    ) -> Result<Vec<EventCenterEvent>> {
        let mut entries = Vec::new();
        let mut offset = (0i64, 0i64);
        loop {
            let req = ApiRequest::new(EVENT_CENTER_API, "List", 1)
                .param("camera_ids", csv(camera_ids))
                .param("start_time", from_time)
                .param("end_time", to_time)
                .param("order_by_start_time", SQL_ORDER_DESC)
                .param(
                    "start_time_offset",
                    json!({ LOCAL_DS_ID: { "start_time": offset.0, "id": offset.1 } }),
                )
                .param("limit", EVENT_CENTER_PAGE_SIZE);
            let data = self.send(&req).await?;
            let page: Vec<EventCenterEvent> = match data.get(LOCAL_DS_ID) {
                Some(v) => decode(v.clone(), "EventCenter.Event.List")?,
                None => Vec::new(),
            };
            let Some(last) = page.last() else {
                return Ok(entries);
            };
            let next = (last.start_time, last.id);
            if next == offset {
                return Ok(entries);
            }
            entries.extend(page);
            offset = next;
        }
    }

    /// `RecordingPicker.EnumInterval`: per camera, the recording files in
    /// `[from_time, to_time]` and the 5-second `event_map`, flattened across
    /// the per-DS groups the NAS returns. Empty `camera_ids` → empty result
    /// without a request.
    pub async fn enum_interval(
        &self,
        camera_ids: &[i64],
        from_time: i64,
        to_time: i64,
    ) -> Result<Vec<IntervalCamera>> {
        if camera_ids.is_empty() {
            return Ok(Vec::new());
        }
        let content = json!([{ "dsId": 0, "archId": 0, "mountId": 0, "camList": camera_ids }]);
        let req = ApiRequest::new(RECORDING_PICKER_API, "EnumInterval", 1)
            .param("from", from_time)
            .param("to", to_time)
            .param("content", content)
            .param("recording", true)
            .param("blStartTimeAsc", true)
            .param("blGetMetaMap", true)
            .param("interval", ENUM_INTERVAL_SEC)
            .param("blExcludeC2", true)
            .timeout(ENUM_INTERVAL_TIMEOUT);
        let data = self.send(&req).await?;
        let mut cameras = Vec::new();
        if let Some(Value::Array(groups)) = data.get("cameras") {
            for group in groups {
                match group {
                    Value::Array(_) => {
                        let cams: Vec<IntervalCamera> =
                            decode(group.clone(), "RecordingPicker.EnumInterval")?;
                        cameras.extend(cams);
                    }
                    Value::Object(_) => {
                        cameras.push(decode(group.clone(), "RecordingPicker.EnumInterval")?);
                    }
                    _ => {}
                }
            }
        }
        Ok(cameras)
    }

    /// `Notification.List` v1.
    pub async fn list_alerts(&self, offset: u32, limit: u32) -> Result<Page<Alert>> {
        let req = ApiRequest::new(NOTIFICATION_API, "List", 1)
            .param("offset", offset)
            .param("limit", limit);
        let data = self.send(&req).await?;
        let raw = list_field(&data, &["notifications", "alerts"])
            .cloned()
            .unwrap_or(Value::Array(vec![]));
        let items: Vec<Alert> = decode(raw, "Notification.List")?;
        let total = total_field(&data, items.len());
        Ok(Page { items, total })
    }

    /// `Notification.GetUnreadCount` v1.
    pub async fn count_unread_alerts(&self) -> Result<i64> {
        let req = ApiRequest::new(NOTIFICATION_API, "GetUnreadCount", 1);
        let data = self.send(&req).await?;
        Ok(data.get("unread").map(crate::de::coerce_int).unwrap_or(0))
    }

    /// `Notification.SetRead` v1 for `alert_ids` (no request when empty).
    pub async fn mark_alerts_read(&self, alert_ids: &[i64]) -> Result<()> {
        if alert_ids.is_empty() {
            return Ok(());
        }
        let req = ApiRequest::new(NOTIFICATION_API, "SetRead", 1).param("idList", csv(alert_ids));
        self.send(&req).await.map(drop)
    }
}
