//! Wall-clock stamps, relative ages and RFC 3339 timestamps in Unix milliseconds.

use std::time::{SystemTime, UNIX_EPOCH};

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;

fn since_epoch() -> std::time::Duration { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default() }

/// Now in Unix milliseconds.
pub fn now_ms() -> i64 { i64::try_from(since_epoch().as_millis()).unwrap_or(i64::MAX) }

/// Now in Unix microseconds.
pub fn now_us() -> u64 { u64::try_from(since_epoch().as_micros()).unwrap_or(u64::MAX) }

/// "just now", "5m ago", "3h ago", "2d ago" for `then_ms` seen at `now_ms`.
pub fn ago(now_ms: i64, then_ms: i64) -> String {
    let minutes = (now_ms - then_ms).max(0) / 60_000;
    match minutes {
        0 => "just now".to_owned(),
        1..=59 => format!("{minutes}m ago"),
        60..=1439 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1440),
    }
}

/// "35m", "4h 35m", "2d 12h" for a span of `ms` milliseconds.
pub fn until(ms: i64) -> String {
    let minutes = (ms / 60_000).max(0);
    let (days, hours, mins) = (minutes / 1440, (minutes / 60) % 24, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{mins}m"),
        (0, _) => format!("{hours}h {mins}m"),
        _ => format!("{days}d {hours}h"),
    }
}

/// An RFC 3339 timestamp (offsets allowed) as Unix milliseconds.
pub fn parse_iso_ms(s: &str) -> Option<i64> {
    let at = OffsetDateTime::parse(s, &Rfc3339).ok()?;
    i64::try_from(at.unix_timestamp_nanos() / 1_000_000).ok()
}

/// Unix milliseconds as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn format_iso_ms(ms: i64) -> String {
    let format = format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .and_then(|at| at.format(&format).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_ages_spans_and_timestamps() {
        let (min, day) = (60_000, 86_400_000);
        let ages = [(-20_000, "just now"), (-5 * min, "5m ago"), (-3 * 60 * min, "3h ago"), (-2 * day, "2d ago"), (min, "just now")];
        ages.iter().for_each(|&(then, want)| assert_eq!(ago(10 * day, 10 * day + then), want));
        [(35 * min, "35m"), (275 * min, "4h 35m"), (2 * day + 12 * 60 * min, "2d 12h")].iter().for_each(|&(ms, want)| assert_eq!(until(ms), want));
        let isos = [("2026-01-02T03:04:05.678Z", Some(1_767_323_045_678)), ("2026-01-02T05:04:05.678+02:00", Some(1_767_323_045_678)), ("1970-01-01T00:00:00Z", Some(0)), ("2026-13-02T03:04:05Z", None)];
        isos.iter().for_each(|&(iso, ms)| assert_eq!(parse_iso_ms(iso), ms, "{iso}"));
        assert_eq!(format_iso_ms(1_767_323_045_678), "2026-01-02T03:04:05.678Z");
        assert!(now_ms() > 1_700_000_000_000 && now_us() / 1000 >= now_ms() as u64 - 1000);
    }
}
