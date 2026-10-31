//! Picking the recording that covers an instant, and turning it into
//! MixStream [`PlayParams`]. Pure functions, unit-tested without a NAS.

use ss_api::Recording;
use ss_stream::{PlayParams, Speed};

/// A `[from, to]` unix-second range for `Recording.List`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub from: i64,
    pub to: i64,
}

/// `[at - half_width, at + half_width]`, never before 0.
pub fn window_around(at: i64, half_width: i64) -> Window {
    let half = half_width.max(0);
    Window {
        from: at.saturating_sub(half).max(0),
        to: at.saturating_add(half),
    }
}

/// Which recording was chosen for an instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection<'a> {
    /// `start_time <= at <= stop_time` (or the recording is still being
    /// written and started before `at`).
    Covering(&'a Recording),
    /// Nothing covers `at`; this is the recording whose edge is closest.
    Nearest {
        rec: &'a Recording,
        /// Seconds between `at` and the nearest edge of `rec`.
        gap_secs: i64,
    },
}

impl Selection<'_> {
    /// The chosen recording either way.
    pub fn recording(&self) -> &Recording {
        match self {
            Selection::Covering(r) | Selection::Nearest { rec: r, .. } => r,
        }
    }
}

/// A recording DSM is still writing reports `stop_time` 0 or before its start.
fn is_open_ended(rec: &Recording) -> bool {
    rec.stop_time <= rec.start_time
}

fn covers(rec: &Recording, at: i64) -> bool {
    at >= rec.start_time && (is_open_ended(rec) || at <= rec.stop_time)
}

/// Distance from `at` to the nearest edge of `rec` (0 when covered).
fn gap(rec: &Recording, at: i64) -> i64 {
    if covers(rec, at) {
        0
    } else if at < rec.start_time {
        rec.start_time - at
    } else {
        at - rec.stop_time
    }
}

/// Choose the recording for `at`: a covering one (the latest-starting if
/// several overlap, so a manual recording wins over the continuous one it
/// was cut out of), else the nearest, else `None` for an empty list.
pub fn select_recording(recs: &[Recording], at: i64) -> Option<Selection<'_>> {
    if let Some(rec) = recs
        .iter()
        .filter(|r| covers(r, at))
        .max_by_key(|r| (r.start_time, r.id))
    {
        return Some(Selection::Covering(rec));
    }
    recs.iter()
        .min_by_key(|r| (gap(r, at), r.start_time))
        .map(|rec| Selection::Nearest {
            rec,
            gap_secs: gap(rec, at),
        })
}

/// The `action=play` parameters to start `rec` at `at`: `start` is
/// seconds into the file (clamped to `[0, length]`), `end` its length.
pub fn play_params(rec: &Recording, at: i64, speed: Speed, reverse: bool) -> PlayParams {
    let length = rec.duration_secs();
    let offset = (at - rec.start_time).clamp(0, length);
    let mut p = PlayParams::new(
        u64::try_from(rec.id).unwrap_or(0),
        rec.mount_id,
        rec.arch_id,
        rec.event_type,
        offset as u64,
        length as u64,
    );
    p.speed = speed;
    p.reverse = reverse;
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: i64, start: i64, stop: i64) -> Recording {
        serde_json::from_value(serde_json::json!({
            "id": id, "cameraId": 1, "startTime": start, "stopTime": stop,
            "mountId": 2, "archId": 3, "type": 4
        }))
        .unwrap()
    }

    #[test]
    fn window_is_symmetric_and_clamped() {
        assert_eq!(
            window_around(10_000, 3600),
            Window {
                from: 6400,
                to: 13_600
            }
        );
        assert_eq!(window_around(100, 3600), Window { from: 0, to: 3700 });
        assert_eq!(window_around(100, -5), Window { from: 100, to: 100 });
    }

    #[test]
    fn covering_recording_is_preferred() {
        let recs = vec![rec(1, 0, 1800), rec(2, 1800, 3600), rec(3, 3600, 5400)];
        assert_eq!(
            select_recording(&recs, 2000),
            Some(Selection::Covering(&recs[1]))
        );
        // Boundaries: the start belongs to the later recording.
        assert_eq!(
            select_recording(&recs, 1800),
            Some(Selection::Covering(&recs[1]))
        );
        assert_eq!(
            select_recording(&recs, 0),
            Some(Selection::Covering(&recs[0]))
        );
        assert_eq!(
            select_recording(&recs, 5400),
            Some(Selection::Covering(&recs[2]))
        );
    }

    #[test]
    fn overlapping_recordings_pick_the_latest_start() {
        let recs = vec![rec(1, 0, 3600), rec(2, 1000, 1300)];
        assert_eq!(
            select_recording(&recs, 1100),
            Some(Selection::Covering(&recs[1]))
        );
        assert_eq!(
            select_recording(&recs, 2000),
            Some(Selection::Covering(&recs[0]))
        );
    }

    #[test]
    fn open_ended_recording_covers_everything_after_its_start() {
        let recs = vec![rec(1, 0, 1800), rec(2, 1800, 0)];
        assert_eq!(
            select_recording(&recs, 99_999),
            Some(Selection::Covering(&recs[1]))
        );
        assert_eq!(
            select_recording(&recs, 100),
            Some(Selection::Covering(&recs[0]))
        );
    }

    #[test]
    fn gap_falls_back_to_the_nearest_edge() {
        let recs = vec![rec(1, 1000, 2000), rec(2, 5000, 6000)];
        assert_eq!(
            select_recording(&recs, 2300),
            Some(Selection::Nearest {
                rec: &recs[0],
                gap_secs: 300
            })
        );
        assert_eq!(
            select_recording(&recs, 4900),
            Some(Selection::Nearest {
                rec: &recs[1],
                gap_secs: 100
            })
        );
        assert_eq!(
            select_recording(&recs, 10),
            Some(Selection::Nearest {
                rec: &recs[0],
                gap_secs: 990
            })
        );
        assert_eq!(select_recording(&[], 10), None);
        assert_eq!(select_recording(&recs, 2300).unwrap().recording().id, 1);
    }

    #[test]
    fn play_params_are_seconds_into_the_file() {
        let r = rec(42, 1000, 1600);
        let p = play_params(&r, 1250, Speed::new(2.0).unwrap(), true);
        assert_eq!(p.recording_id, 42);
        assert_eq!((p.mount_id, p.arch_id, p.event_type), (2, 3, 4));
        assert_eq!(p.start_s, 250);
        assert_eq!(p.end_s, 600);
        assert_eq!(p.speed.multiplier(), 2.0);
        assert!(p.reverse && !p.paused && !p.mute);
        // Before the start and after the end are clamped.
        assert_eq!(play_params(&r, 10, Speed::NORMAL, false).start_s, 0);
        assert_eq!(play_params(&r, 9999, Speed::NORMAL, false).start_s, 600);
        // An open-ended recording has length 0 (DSM clamps start to end).
        let open = rec(7, 1000, 0);
        let p = play_params(&open, 1300, Speed::NORMAL, false);
        assert_eq!((p.start_s, p.end_s), (0, 0));
    }
}
