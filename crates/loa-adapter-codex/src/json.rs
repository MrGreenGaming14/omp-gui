//! Small helpers over `serde_json` values and RFC 3339 timestamps.

use serde_json::{Map, Value};

/// The string at `key`, when `value` is an object holding a string there.
pub(crate) fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

/// The integer at `key`.
pub(crate) fn int_at(value: &Value, key: &str) -> Option<i64> {
    value.get(key)?.as_i64()
}

/// Removes `key` from `object` when it holds a string and returns the string.
pub(crate) fn take_str(object: &mut Map<String, Value>, key: &str) -> Option<String> {
    match object.get(key) {
        Some(Value::String(_)) => match object.remove(key) {
            Some(Value::String(text)) => Some(text),
            _ => None,
        },
        _ => None,
    }
}

/// Parses an RFC 3339 timestamp (`2026-01-02T03:04:05.678Z`, offsets allowed)
/// into unix milliseconds.
pub(crate) fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || !matches!(bytes[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> { text.get(range)?.parse::<i64>().ok() };
    let year = num(0..4)?;
    let month = u32::try_from(num(5..7)?).ok()?;
    let day = u32::try_from(num(8..10)?).ok()?;
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let hour = num(11..13)?;
    let minute = num(14..16)?;
    let second = num(17..19)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut rest = &text[19..];
    let mut millis = 0i64;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let mut scale = 100;
        for digit in fraction[..digits].bytes().take(3) {
            millis += i64::from(digit - b'0') * scale;
            scale /= 10;
        }
        rest = &fraction[digits..];
    }
    let offset_minutes = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            if rest.len() != 6 || rest.as_bytes()[3] != b':' {
                return None;
            }
            let hours: i64 = rest[1..3].parse().ok()?;
            let minutes: i64 = rest[4..6].parse().ok()?;
            sign * (hours * 60 + minutes)
        }
    };
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_minutes * 60;
    Some(seconds * 1_000 + millis)
}

/// Formats unix milliseconds as `YYYY-MM-DDTHH:MM:SS.mmmZ`, the form Codex writes.
pub(crate) fn format_rfc3339_ms(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let in_day = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        in_day / 3_600_000,
        in_day / 60_000 % 60,
        in_day / 1_000 % 60,
        in_day % 1_000
    )
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip_through_unix_millis() {
        let ms = parse_rfc3339_ms("2026-03-01T00:00:00.250Z").expect("valid timestamp");
        assert_eq!(format_rfc3339_ms(ms), "2026-03-01T00:00:00.250Z");
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:01Z"), Some(1_000));
        assert_eq!(parse_rfc3339_ms("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_rfc3339_ms("2024-02-29T12:00:00.123456Z"), Some(1_709_208_000_123));
        assert_eq!(parse_rfc3339_ms("yesterday"), None);
    }
}
