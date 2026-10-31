//! The synthetic Surveillance Station: six cameras, 48 h of half-hour
//! recordings per enabled camera, motion/alarm/manual events, snapshots,
//! alerts, licences and time-lapse tasks.
//!
//! Everything time-based is a pure function of `(seed, camera, period)`,
//! so any window can be answered without storing it and the same ids come
//! back on every call. What the client can change (alerts read, snapshots
//! taken/deleted, licences, home mode, time-lapse locks) lives in a small
//! mutex.
//!
//! Recording ids encode their position: `id = period * 10 + camera_id`,
//! `period = start_time / 1800`.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::rng::Rng;

/// Length of one recording file.
pub const PERIOD_SECS: i64 = 1800;
/// How far back recordings exist.
pub const HORIZON_SECS: i64 = 48 * 3600;
/// Surveillance Station version reported by `Info.GetInfo`.
pub const SS_VERSION: (u32, u32, u32, u32) = (9, 3, 1, 11180);
pub const DS_MODEL: &str = "DS923+";
pub const HOSTNAME: &str = "fake-nas";
pub const SERIAL: &str = "2340TGR9FAKE1";
/// Free camera licences on the model.
pub const FREE_LICENSES: i64 = 2;
pub const MAX_CAMERAS: i64 = 40;

/// Unix seconds now.
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug)]
pub struct CameraDef {
    pub id: i64,
    pub name: &'static str,
    pub vendor: &'static str,
    pub model: &'static str,
    pub ip: &'static str,
    /// `Camera.List` `status`: 1 enabled, 3 disconnected, 7 disabled.
    pub status: i64,
    pub ptz: bool,
    pub audio: bool,
    /// `videoCodec`: 3 H.264, 5 H.265 (what the camera *list* advertises;
    /// the stream is always H.264 bytes).
    pub video_codec: i64,
    /// `defLiveProfile`: 0 high, 1 balanced, 2 low.
    pub def_live_profile: i64,
    pub resolution: &'static str,
}

impl CameraDef {
    pub fn enabled(&self) -> bool {
        self.status == 1
    }
}

pub static CAMERAS: [CameraDef; 6] = [
    CameraDef {
        id: 1,
        name: "Front Door",
        vendor: "Axis",
        model: "P3265-LVE",
        ip: "192.168.1.11",
        status: 1,
        ptz: true,
        audio: true,
        video_codec: 3,
        def_live_profile: 1,
        resolution: "1920x1080",
    },
    CameraDef {
        id: 2,
        name: "Driveway",
        vendor: "Hikvision",
        model: "DS-2CD2387G2-LU",
        ip: "192.168.1.12",
        status: 1,
        ptz: false,
        audio: true,
        video_codec: 5,
        def_live_profile: 0,
        resolution: "3840x2160",
    },
    CameraDef {
        id: 3,
        name: "Garden",
        vendor: "Reolink",
        model: "RLC-810A",
        ip: "192.168.1.13",
        status: 1,
        ptz: false,
        audio: true,
        video_codec: 3,
        def_live_profile: 1,
        resolution: "2560x1440",
    },
    CameraDef {
        id: 4,
        name: "Garage",
        vendor: "Dahua",
        model: "IPC-HDW3841T",
        ip: "192.168.1.14",
        status: 1,
        ptz: false,
        audio: false,
        video_codec: 3,
        def_live_profile: 2,
        resolution: "1920x1080",
    },
    CameraDef {
        id: 5,
        name: "Back Yard",
        vendor: "Synology",
        model: "BC500",
        ip: "192.168.1.15",
        status: 1,
        ptz: false,
        audio: true,
        video_codec: 3,
        def_live_profile: 1,
        resolution: "2880x1620",
    },
    CameraDef {
        id: 6,
        name: "Side Gate",
        vendor: "TP-Link",
        model: "VIGI C340",
        ip: "192.168.1.16",
        status: 7,
        ptz: false,
        audio: false,
        video_codec: 3,
        def_live_profile: 1,
        resolution: "1920x1080",
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Motion,
    Alarm,
    Manual,
}

impl EventKind {
    /// The recording-mode number (`Recording.List` `type`, `recEvtType`).
    pub fn rec_type(self) -> i64 {
        match self {
            Self::Motion => 1,
            Self::Alarm => 2,
            Self::Manual => 3,
        }
    }

    /// `(event_type, object_type)` as `EventCenter.Event.List` types them:
    /// 10 motion, 19 digital input (alarm), 11/2 person detection (what a
    /// manually tagged event is shown as here, so the UI gets a third kind).
    pub fn event_center_type(self) -> (i64, i64) {
        match self {
            Self::Motion => (10, 0),
            Self::Alarm => (19, 0),
            Self::Manual => (11, 2),
        }
    }

    /// `event_map` flag: bit 0 "recording" plus one detection bit (bit 8
    /// is the reference's motion bit).
    pub fn map_flag(self) -> i64 {
        match self {
            Self::Motion => 1 | (1 << 8),
            Self::Alarm => 1 | (1 << 9),
            Self::Manual => 1 | (1 << 10),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingDef {
    pub id: i64,
    pub camera_id: i64,
    pub start: i64,
    pub stop: i64,
    /// Still being written (the current half hour).
    pub recording: bool,
    /// `defaultLabel` bitmask (bit 1 person, bit 2 vehicle).
    pub label: i64,
}

impl RecordingDef {
    pub fn duration_secs(&self) -> i64 {
        (self.stop - self.start).max(0)
    }

    pub fn file_size(&self) -> i64 {
        self.duration_secs() * 180_000
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventDef {
    pub id: i64,
    pub camera_id: i64,
    pub start: i64,
    pub duration: i64,
    pub kind: EventKind,
}

#[derive(Clone, Debug)]
pub struct Alert {
    pub id: i64,
    pub camera_id: i64,
    pub alert_type: i64,
    pub timestamp: i64,
    pub is_read: bool,
}

#[derive(Clone, Debug)]
pub struct SnapshotDef {
    pub id: i64,
    pub camera_id: i64,
    pub created: i64,
}

#[derive(Clone, Debug)]
pub struct LicenseKey {
    pub id: i64,
    pub key: String,
    pub quota: i64,
}

#[derive(Clone, Debug)]
pub struct TimeLapseTask {
    pub id: i64,
    pub name: &'static str,
    pub camera_id: i64,
}

#[derive(Clone, Debug)]
pub struct TimeLapseRec {
    pub id: i64,
    pub task_id: i64,
    pub camera_id: i64,
    pub start: i64,
    pub stop: i64,
    pub recording: bool,
    pub locked: bool,
}

pub const TIMELAPSE_TASKS: [TimeLapseTask; 2] = [
    TimeLapseTask {
        id: 1,
        name: "Garden day lapse",
        camera_id: 3,
    },
    TimeLapseTask {
        id: 2,
        name: "Driveway lapse",
        camera_id: 2,
    },
];

struct State {
    home_mode: bool,
    alerts: Vec<Alert>,
    snapshots: Vec<SnapshotDef>,
    next_snapshot_id: i64,
    licenses: Vec<LicenseKey>,
    next_license_id: i64,
    timelapse_deleted: HashSet<i64>,
    timelapse_locked: HashSet<i64>,
}

pub struct World {
    seed: u64,
    /// When the server started (unix seconds); anchors the fixed lists.
    pub started: i64,
    state: Mutex<State>,
}

impl World {
    pub fn new(seed: u64) -> World {
        let started = now_secs();
        let snapshots = (0..8)
            .map(|k| SnapshotDef {
                id: k + 1,
                camera_id: [1, 2, 3, 4, 5, 1, 2, 3][k as usize],
                created: started - 120 - k * 3700,
            })
            .collect();
        let alerts = vec![
            Alert {
                id: 1,
                camera_id: 1,
                alert_type: 0,
                timestamp: started - 900,
                is_read: false,
            },
            Alert {
                id: 2,
                camera_id: 3,
                alert_type: 3,
                timestamp: started - 5400,
                is_read: true,
            },
            Alert {
                id: 3,
                camera_id: 6,
                alert_type: 1,
                timestamp: started - 20_000,
                is_read: true,
            },
        ];
        let licenses = vec![LicenseKey {
            id: 1,
            key: "FAKE-NAS0-4CAM-PACK".into(),
            quota: 4,
        }];
        World {
            seed,
            started,
            state: Mutex::new(State {
                home_mode: false,
                alerts,
                snapshots,
                next_snapshot_id: 9,
                licenses,
                next_license_id: 2,
                timelapse_deleted: HashSet::new(),
                timelapse_locked: HashSet::from([9011]),
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // ----- cameras -------------------------------------------------------

    pub fn cameras(&self) -> &'static [CameraDef] {
        &CAMERAS
    }

    pub fn camera(&self, id: i64) -> Option<&'static CameraDef> {
        CAMERAS.iter().find(|c| c.id == id)
    }

    pub fn camera_name(&self, id: i64) -> &'static str {
        self.camera(id).map_or("", |c| c.name)
    }

    pub fn enabled_camera_count(&self) -> i64 {
        CAMERAS.iter().filter(|c| c.enabled()).count() as i64
    }

    // ----- recordings ----------------------------------------------------

    fn period_range(&self, now: i64) -> (i64, i64) {
        let horizon = now - HORIZON_SECS;
        let first = (horizon + PERIOD_SECS - 1).div_euclid(PERIOD_SECS);
        let last = now.div_euclid(PERIOD_SECS);
        (first, last)
    }

    fn recording_for(&self, camera_id: i64, period: i64, now: i64) -> RecordingDef {
        let start = period * PERIOD_SECS;
        let current = period == now.div_euclid(PERIOD_SECS);
        let stop = if current { now } else { start + PERIOD_SECS };
        let mut rng = Rng::from_parts(&[self.seed, 0xEC, camera_id as u64, period as u64]);
        let label = match rng.below(100) {
            0..=29 => 2,
            30..=44 => 4,
            45..=54 => 6,
            _ => 0,
        };
        RecordingDef {
            id: period * 10 + camera_id,
            camera_id,
            start,
            stop,
            recording: current,
            label,
        }
    }

    /// Recordings of `camera_id` overlapping `[from, to]`, oldest first.
    pub fn recordings(&self, camera_id: i64, from: i64, to: i64, now: i64) -> Vec<RecordingDef> {
        let Some(cam) = self.camera(camera_id) else {
            return Vec::new();
        };
        if !cam.enabled() {
            return Vec::new();
        }
        let (first, last) = self.period_range(now);
        let lo = first.max(from.div_euclid(PERIOD_SECS));
        let hi = last.min(to.div_euclid(PERIOD_SECS));
        (lo..=hi)
            .map(|p| self.recording_for(camera_id, p, now))
            .filter(|r| r.start <= to && r.stop >= from)
            .collect()
    }

    /// The recording with `id`, if it exists right now.
    pub fn recording_by_id(&self, id: i64, now: i64) -> Option<RecordingDef> {
        let camera_id = id.rem_euclid(10);
        let period = id.div_euclid(10);
        let cam = self.camera(camera_id)?;
        if !cam.enabled() {
            return None;
        }
        let (first, last) = self.period_range(now);
        (first..=last)
            .contains(&period)
            .then(|| self.recording_for(camera_id, period, now))
    }

    /// Camera and length in seconds of something `action=play` can name:
    /// a recording or a time-lapse file.
    pub fn playable(&self, id: i64, now: i64) -> Option<(&'static CameraDef, i64)> {
        if let Some(r) = self.recording_by_id(id, now) {
            return Some((self.camera(r.camera_id)?, r.duration_secs()));
        }
        let tl = self
            .timelapse_recordings(None)
            .into_iter()
            .find(|r| r.id == id)?;
        Some((self.camera(tl.camera_id)?, (tl.stop - tl.start).max(0)))
    }

    // ----- events --------------------------------------------------------

    fn events_in_period(&self, camera_id: i64, period: i64, now: i64) -> Vec<EventDef> {
        let mut rng = Rng::from_parts(&[self.seed, 0xE7, camera_id as u64, period as u64]);
        let count = if rng.chance(0.35) { 2 } else { 1 };
        let mut events: Vec<EventDef> = (0..count)
            .map(|k| {
                let start = period * PERIOD_SECS + rng.range(0, PERIOD_SECS);
                let duration = rng.range(10, 91);
                let kind = match rng.below(100) {
                    0..=7 => EventKind::Alarm,
                    8..=13 => EventKind::Manual,
                    _ => EventKind::Motion,
                };
                EventDef {
                    id: (period * 10 + camera_id) * 10 + k,
                    camera_id,
                    start,
                    duration,
                    kind,
                }
            })
            .filter(|e| e.start <= now)
            .collect();
        events.sort_by_key(|e| e.start);
        events
    }

    /// Events of `camera_id` overlapping `[from, to]`, oldest first.
    pub fn events(&self, camera_id: i64, from: i64, to: i64, now: i64) -> Vec<EventDef> {
        let Some(cam) = self.camera(camera_id) else {
            return Vec::new();
        };
        if !cam.enabled() {
            return Vec::new();
        }
        let (first, last) = self.period_range(now);
        let lo = first.max(from.div_euclid(PERIOD_SECS));
        let hi = last.min(to.div_euclid(PERIOD_SECS));
        (lo..=hi)
            .flat_map(|p| self.events_in_period(camera_id, p, now))
            .filter(|e| e.start <= to && e.start + e.duration >= from)
            .collect()
    }

    /// `RecordingPicker.EnumInterval`'s run-length `event_map` for
    /// `[from, to]` in `interval`-second buckets: `[count, flag, reserved]`
    /// with flag 0 = no recording, 1 = recording, else recording + a
    /// detection bit ([`EventKind::map_flag`]).
    pub fn event_map(
        &self,
        camera_id: i64,
        from: i64,
        to: i64,
        interval: i64,
        now: i64,
    ) -> Vec<[i64; 3]> {
        let interval = interval.max(1);
        let enabled = self.camera(camera_id).is_some_and(CameraDef::enabled);
        let (first, _) = self.period_range(now);
        let rec_start = first * PERIOD_SECS;
        let events = self.events(camera_id, from, to, now);
        let mut runs: Vec<[i64; 3]> = Vec::new();
        let mut t = from;
        let mut ei = 0usize;
        while t < to {
            let flag = if !enabled || t < rec_start || t >= now {
                0
            } else {
                while ei < events.len() && events[ei].start + events[ei].duration <= t {
                    ei += 1;
                }
                events[ei..]
                    .iter()
                    .take_while(|e| e.start < t + interval)
                    .find(|e| e.start < t + interval && e.start + e.duration > t)
                    .map_or(1, |e| e.kind.map_flag())
            };
            match runs.last_mut() {
                Some(last) if last[1] == flag => last[0] += 1,
                _ => runs.push([1, flag, 0]),
            }
            t += interval;
        }
        runs
    }

    // ----- mutable state -------------------------------------------------

    pub fn home_mode(&self) -> bool {
        self.state().home_mode
    }

    pub fn set_home_mode(&self, on: bool) {
        self.state().home_mode = on;
    }

    pub fn alerts(&self) -> Vec<Alert> {
        self.state().alerts.clone()
    }

    pub fn unread_alerts(&self) -> i64 {
        self.state().alerts.iter().filter(|a| !a.is_read).count() as i64
    }

    pub fn mark_alerts_read(&self, ids: &[i64]) {
        for a in &mut self.state().alerts {
            if ids.contains(&a.id) {
                a.is_read = true;
            }
        }
    }

    pub fn snapshots(&self) -> Vec<SnapshotDef> {
        self.state().snapshots.clone()
    }

    pub fn snapshot(&self, id: i64) -> Option<SnapshotDef> {
        self.state().snapshots.iter().find(|s| s.id == id).cloned()
    }

    pub fn take_snapshot(&self, camera_id: i64) -> i64 {
        let mut st = self.state();
        let id = st.next_snapshot_id;
        st.next_snapshot_id += 1;
        st.snapshots.push(SnapshotDef {
            id,
            camera_id,
            created: now_secs(),
        });
        id
    }

    pub fn delete_snapshots(&self, ids: &[i64]) -> usize {
        let mut st = self.state();
        let before = st.snapshots.len();
        st.snapshots.retain(|s| !ids.contains(&s.id));
        before - st.snapshots.len()
    }

    pub fn licenses(&self) -> Vec<LicenseKey> {
        self.state().licenses.clone()
    }

    pub fn add_licenses(&self, keys: &[&str]) {
        let mut st = self.state();
        for k in keys.iter().filter(|k| !k.trim().is_empty()) {
            let id = st.next_license_id;
            st.next_license_id += 1;
            st.licenses.push(LicenseKey {
                id,
                key: k.trim().to_owned(),
                quota: 1,
            });
        }
    }

    pub fn delete_licenses(&self, ids: &[i64]) {
        self.state().licenses.retain(|l| !ids.contains(&l.id));
    }

    pub fn timelapse_recordings(&self, range: Option<(i64, i64)>) -> Vec<TimeLapseRec> {
        let st = self.state();
        let midnight = self.started - self.started.rem_euclid(86_400);
        let mut out = Vec::new();
        for task in &TIMELAPSE_TASKS {
            for d in 0..3i64 {
                let id = 9000 + task.id * 10 + d;
                if st.timelapse_deleted.contains(&id) {
                    continue;
                }
                let start = midnight - d * 86_400;
                let (stop, recording) = if d == 0 {
                    (self.started, true)
                } else {
                    (start + 86_400, false)
                };
                if let Some((from, to)) = range
                    && (start > to || stop < from)
                {
                    continue;
                }
                out.push(TimeLapseRec {
                    id,
                    task_id: task.id,
                    camera_id: task.camera_id,
                    start,
                    stop,
                    recording,
                    locked: st.timelapse_locked.contains(&id),
                });
            }
        }
        out
    }

    pub fn set_timelapse_locked(&self, ids: &[i64], locked: bool) {
        let mut st = self.state();
        for id in ids {
            if locked {
                st.timelapse_locked.insert(*id);
            } else {
                st.timelapse_locked.remove(id);
            }
        }
    }

    pub fn delete_timelapse(&self, ids: &[i64]) {
        let mut st = self.state();
        for id in ids {
            if !st.timelapse_locked.contains(id) {
                st.timelapse_deleted.insert(*id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_761_900_000; // 2025-10-31-ish, arbitrary

    #[test]
    fn recordings_cover_48h_in_half_hours() {
        let w = World::new(1);
        let recs = w.recordings(1, NOW - HORIZON_SECS, NOW, NOW);
        assert!(recs.len() == 96 || recs.len() == 97, "{}", recs.len());
        assert!(recs.windows(2).all(|p| p[0].stop == p[1].start));
        assert!(recs.last().unwrap().recording);
        assert_eq!(recs.last().unwrap().stop, NOW);
        for r in &recs {
            assert_eq!(w.recording_by_id(r.id, NOW).as_ref(), Some(r));
        }
        assert!(w.recordings(6, NOW - 3600, NOW, NOW).is_empty());
        assert!(w.recording_by_id(7, NOW).is_none());
    }

    #[test]
    fn events_are_deterministic_and_in_range() {
        let w = World::new(1);
        let a = w.events(2, NOW - 7200, NOW, NOW);
        let b = World::new(1).events(2, NOW - 7200, NOW, NOW);
        assert_eq!(a, b);
        assert!(!a.is_empty());
        assert!(
            a.iter()
                .all(|e| e.start <= NOW && e.start + e.duration >= NOW - 7200)
        );
        assert!(a.iter().all(|e| (10..=90).contains(&e.duration)));
        assert_ne!(a, World::new(2).events(2, NOW - 7200, NOW, NOW));
    }

    #[test]
    fn event_map_runs_sum_to_the_window() {
        let w = World::new(1);
        let from = NOW - 7200;
        let map = w.event_map(1, from, NOW, 5, NOW);
        assert_eq!(map.iter().map(|r| r[0]).sum::<i64>() * 5, 7200);
        assert!(map.iter().any(|r| r[1] > 1), "no event buckets: {map:?}");
        assert!(map.iter().all(|r| r[2] == 0));
        // disabled camera: one run of flag 0
        let off = w.event_map(6, from, NOW, 5, NOW);
        assert_eq!(off, vec![[1440, 0, 0]]);
    }

    #[test]
    fn mutable_state_round_trips() {
        let w = World::new(1);
        assert_eq!(w.unread_alerts(), 1);
        w.mark_alerts_read(&[1]);
        assert_eq!(w.unread_alerts(), 0);
        assert_eq!(w.snapshots().len(), 8);
        let id = w.take_snapshot(2);
        assert_eq!(w.snapshots().len(), 9);
        assert_eq!(w.delete_snapshots(&[id, 1]), 2);
        assert_eq!(w.licenses().len(), 1);
        w.add_licenses(&["AAAA", ""]);
        assert_eq!(w.licenses().len(), 2);
        w.delete_licenses(&[1]);
        assert_eq!(w.licenses()[0].key, "AAAA");
        assert_eq!(w.timelapse_recordings(None).len(), 6);
        w.delete_timelapse(&[9011, 9012]);
        assert_eq!(w.timelapse_recordings(None).len(), 5); // 9011 is locked
        assert!(w.playable(9011, now_secs()).is_some());
    }
}
