//! `SYNO.SurveillanceStation.Recording`: list, download, playback URL.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::io::AsyncWrite;
use url::Url;

use crate::client::{ApiRequest, Client, decode, list_field, total_field};
use crate::error::{Error, Result};
use crate::models::{Page, Recording};

/// The recording API name.
pub const RECORDING_API: &str = "SYNO.SurveillanceStation.Recording";
/// `Recording.Download` by recording id needs version 6 or later; version 5
/// answers 400 "Execution failed" with no file. (The official web client's
/// version 4 call uses an event-based parameter set for sub-ranges, see
/// [`Client::download_recording_range`].)
pub const RECORDING_DOWNLOAD_VERSION: u32 = 6;

/// Filters for [`Client::list_recordings`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingFilter {
    /// Restrict to these cameras (`cameraIds`); empty = all.
    pub camera_ids: Vec<i64>,
    /// Unix seconds (`fromTime`).
    pub from_time: Option<i64>,
    /// Unix seconds (`toTime`).
    pub to_time: Option<i64>,
    pub offset: u32,
    pub limit: u32,
}

impl Default for RecordingFilter {
    fn default() -> Self {
        Self {
            camera_ids: Vec::new(),
            from_time: None,
            to_time: None,
            offset: 0,
            limit: 50,
        }
    }
}

impl RecordingFilter {
    /// Recordings of one camera.
    pub fn camera(camera_id: i64) -> Self {
        Self {
            camera_ids: vec![camera_id],
            ..Default::default()
        }
    }

    /// Restrict to `[from, to]` (unix seconds).
    pub fn between(mut self, from: i64, to: i64) -> Self {
        self.from_time = Some(from);
        self.to_time = Some(to);
        self
    }

    /// Page position.
    pub fn page(mut self, offset: u32, limit: u32) -> Self {
        self.offset = offset;
        self.limit = limit;
        self
    }
}

impl Client {
    /// `Recording.List` v5. The answer's list is under `events` (older:
    /// `recordings`).
    pub async fn list_recordings(&self, filter: &RecordingFilter) -> Result<Page<Recording>> {
        let mut req = ApiRequest::new(RECORDING_API, "List", 5)
            .param("offset", filter.offset)
            .param("limit", filter.limit);
        if !filter.camera_ids.is_empty() {
            let ids = filter
                .camera_ids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            req = req.param("cameraIds", ids);
        }
        req = req
            .param_opt("fromTime", filter.from_time)
            .param_opt("toTime", filter.to_time);
        let data = self.send(&req).await?;
        let raw = list_field(&data, &["events", "recordings"])
            .cloned()
            .unwrap_or(Value::Array(vec![]));
        let items: Vec<Recording> = decode(raw, "Recording.List")?;
        let total = total_field(&data, items.len());
        Ok(Page { items, total })
    }

    /// `Recording.Download` v6 by recording id, streamed into `out`.
    /// Returns the number of bytes written.
    pub async fn download_recording<W: AsyncWrite + Unpin + ?Sized>(
        &self,
        recording_id: i64,
        out: &mut W,
    ) -> Result<u64> {
        let req = ApiRequest::new(RECORDING_API, "Download", RECORDING_DOWNLOAD_VERSION)
            .param("id", recording_id);
        self.download_to(&req, out).await
    }

    /// Download the `[start_unix, end_unix)` slice of `rec` with the
    /// event-based `Recording.Download` v4 (`offsetTimeMs`/`playTimeMs`), so
    /// DSM cuts the sub-range itself. The range is clamped to what the
    /// recording holds; [`Error::InvalidArgument`] when nothing is left.
    pub async fn download_recording_range<W: AsyncWrite + Unpin + ?Sized>(
        &self,
        rec: &Recording,
        start_unix: f64,
        end_unix: f64,
        out: &mut W,
    ) -> Result<u64> {
        let (offset_ms, play_ms) = range_to_offsets(rec, start_unix, end_unix)?;
        let req = ApiRequest::new(RECORDING_API, "Download", 4)
            .param("eventId", rec.id)
            .param("offsetTimeMs", offset_ms)
            .param("playTimeMs", play_ms)
            .param("mountId", rec.mount_id)
            .param("archId", rec.arch_id)
            .param("recEvtType", rec.event_type);
        self.download_to(&req, out).await
    }

    /// A playback URL for `rec`: `SYNO.SurveillanceStation.Stream`
    /// `EventStream` when discovery found it, else the legacy
    /// `SYNO.SurveillanceStation.Streaming` `EventStream`. Carries `_sid`.
    pub fn recording_stream_url(&self, rec: &Recording) -> Url {
        const STREAM: &str = "SYNO.SurveillanceStation.Stream";
        const LEGACY: &str = "SYNO.SurveillanceStation.Streaming";
        let (id, mount) = (rec.id.to_string(), rec.mount_id.to_string());
        if self.has_api(STREAM) {
            let version = self.api_version(STREAM, Some(1)).to_string();
            let (rec_type, arch) = (rec.event_type.to_string(), rec.arch_id.to_string());
            return self.stream_url(
                STREAM,
                &[
                    ("api", STREAM),
                    ("method", "EventStream"),
                    ("version", &version),
                    ("eventId", &id),
                    ("mountId", &mount),
                    ("recEvtType", &rec_type),
                    ("archId", &arch),
                ],
            );
        }
        let version = self.api_version(LEGACY, Some(2)).to_string();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            .to_string();
        self.stream_url(
            LEGACY,
            &[
                ("api", LEGACY),
                ("method", "EventStream"),
                ("version", &version),
                ("eventId", &id),
                ("mountId", &mount),
                ("framestart", "0"),
                ("timestamp", &now),
            ],
        )
    }
}

/// `(offsetTimeMs, playTimeMs)` for the `[start, end)` slice of `rec`.
fn range_to_offsets(rec: &Recording, start_unix: f64, end_unix: f64) -> Result<(i64, i64)> {
    let rec_start = rec.start_time as f64;
    let rec_stop = rec.stop_time as f64;
    let offset_ms = ((start_unix - rec_start) * 1000.0).round().max(0.0) as i64;
    if end_unix <= start_unix {
        return Err(Error::InvalidArgument(
            "end time must be after start time".into(),
        ));
    }
    // A range that begins before the recording only has footage from the
    // recording's start on.
    let play_ms = ((end_unix - start_unix.max(rec_start)) * 1000.0).round() as i64;
    if play_ms <= 0 {
        return Err(Error::InvalidArgument(
            "selected range ends before the recording starts".into(),
        ));
    }
    // A recording DSM is still writing has a stale stop_time; never ask
    // for more than it holds.
    let available_ms = ((rec_stop - rec_start) * 1000.0).round().max(0.0) as i64 - offset_ms;
    if available_ms <= 0 {
        return Err(Error::InvalidArgument(
            "selected range starts after the end of the recording".into(),
        ));
    }
    Ok((offset_ms, play_ms.min(available_ms)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec() -> Recording {
        serde_json::from_value(serde_json::json!({
            "id": 7, "cameraId": 1, "startTime": 1000, "stopTime": 1600,
            "mountId": 0, "archId": 2, "type": 1
        }))
        .unwrap()
    }

    #[test]
    fn range_math() {
        assert_eq!(
            range_to_offsets(&rec(), 1100.0, 1200.0).unwrap(),
            (100_000, 100_000)
        );
        // Starts before the recording: offset 0, play counted from rec start.
        assert_eq!(
            range_to_offsets(&rec(), 900.0, 1200.0).unwrap(),
            (0, 200_000)
        );
        // Runs past the end: clamped to what exists.
        assert_eq!(
            range_to_offsets(&rec(), 1500.0, 1900.0).unwrap(),
            (500_000, 100_000)
        );
        assert!(matches!(
            range_to_offsets(&rec(), 1200.0, 1200.0),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            range_to_offsets(&rec(), 800.0, 900.0),
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            range_to_offsets(&rec(), 1700.0, 1800.0),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn legacy_stream_url_without_discovery() {
        let c = Client::new("https://nas:5001").unwrap();
        let url = c.recording_stream_url(&rec());
        let s = url.as_str();
        assert!(s.contains("api=SYNO.SurveillanceStation.Streaming"));
        assert!(s.contains("method=EventStream") && s.contains("eventId=7"));
        assert!(s.contains("framestart=0") && s.contains("version=2"));
    }
}
