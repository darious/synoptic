//! Local-time parsing and formatting for the command line.
//!
//! Surveillance Station's API speaks unix seconds; users type wall-clock
//! time in their own zone. Accepted input: `YYYY-MM-DD HH:MM:SS`,
//! `YYYY-MM-DD HH:MM`, the same with a `T` separator, a bare `YYYY-MM-DD`
//! (midnight), or an integer that is already unix seconds.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone};

const FORMATS: &[&str] = &[
    "%Y-%m-%d %H:%M:%S",
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%d %H:%M",
    "%Y-%m-%dT%H:%M",
];

/// Parse `text` as a wall-clock time in the machine's local zone (or as
/// unix seconds) and return unix seconds.
pub fn parse_local(text: &str) -> Result<i64> {
    parse_in(text, &Local)
}

/// [`parse_local`] for any zone; what the tests use with a fixed offset.
pub fn parse_in<Tz: TimeZone>(text: &str, tz: &Tz) -> Result<i64> {
    let text = text.trim();
    if text.is_empty() {
        bail!("empty time");
    }
    if text.bytes().all(|b| b.is_ascii_digit()) && text.len() >= 9 {
        return text
            .parse::<i64>()
            .with_context(|| format!("not a unix timestamp: {text:?}"));
    }
    let naive = FORMATS
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(text, f).ok())
        .or_else(|| {
            NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
        })
        .ok_or_else(|| {
            anyhow!("cannot parse {text:?}: expected \"YYYY-MM-DD HH:MM:SS\" or unix seconds")
        })?;
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(t) => Ok(t.timestamp()),
        // Clocks went back: the earlier of the two instants, as the reference
        // (datetime.fromisoformat, fold=0) picks.
        LocalResult::Ambiguous(earlier, _) => Ok(earlier.timestamp()),
        LocalResult::None => bail!("{text:?} does not exist in the local time zone (DST gap)"),
    }
}

/// `YYYY-MM-DD HH:MM:SS` in the local zone.
pub fn format_local(unix: i64) -> String {
    format_in(unix, &Local)
}

/// [`format_local`] for any zone.
pub fn format_in<Tz: TimeZone>(unix: i64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match DateTime::from_timestamp(unix, 0) {
        Some(t) => t.with_timezone(tz).format("%Y-%m-%d %H:%M:%S").to_string(),
        None => unix.to_string(),
    }
}

/// `1h02m03s`, `45s`, `0s`.
pub fn format_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m:02}m{s:02}s")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn plus_two() -> FixedOffset {
        FixedOffset::east_opt(2 * 3600).unwrap()
    }

    #[test]
    fn parses_wall_clock_in_the_given_zone() {
        let tz = plus_two();
        // 2026-10-30 21:15:00 +02:00 == 19:15:00 UTC
        let expected = 1_793_387_700;
        assert_eq!(parse_in("2026-10-30 21:15:00", &tz).unwrap(), expected);
        assert_eq!(parse_in("2026-10-30T21:15:00", &tz).unwrap(), expected);
        assert_eq!(parse_in("2026-10-30 21:15", &tz).unwrap(), expected);
        assert_eq!(parse_in(" 2026-10-30 21:15:00 ", &tz).unwrap(), expected);
        assert_eq!(
            parse_in("2026-10-30", &tz).unwrap(),
            expected - 21 * 3600 - 15 * 60
        );
        assert_eq!(
            parse_in("2026-10-30 21:15:00", &chrono::Utc).unwrap(),
            expected + 7200
        );
    }

    #[test]
    fn unix_seconds_pass_through() {
        assert_eq!(parse_in("1793387700", &plus_two()).unwrap(), 1_793_387_700);
    }

    #[test]
    fn garbage_is_an_error() {
        for bad in [
            "",
            "yesterday",
            "2026-13-01 00:00:00",
            "30/10/2026",
            "12345",
        ] {
            assert!(parse_in(bad, &plus_two()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn formats_round_trip() {
        let tz = plus_two();
        let t = parse_in("2026-10-30 21:15:07", &tz).unwrap();
        assert_eq!(format_in(t, &tz), "2026-10-30 21:15:07");
        assert_eq!(format_in(t, &chrono::Utc), "2026-10-30 19:15:07");
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(125), "2m05s");
        assert_eq!(format_duration(3723), "1h02m03s");
        assert_eq!(format_duration(-5), "0s");
    }
}
