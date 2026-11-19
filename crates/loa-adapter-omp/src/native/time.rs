//! ISO-8601 timestamps as omp writes them (`2026-02-16T10:20:30.000Z`).

const MS_PER_DAY: i64 = 86_400_000;

/// Unix milliseconds of an ISO-8601 date-time: any number of fractional
/// digits (or none) and `Z`, `±HH:MM`, `±HHMM`, `±HH` or no offset (UTC).
pub(super) fn parse_iso(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    let separators = b.len() >= 19
        && b[4] == b'-'
        && b[7] == b'-'
        && matches!(b[10], b'T' | b't' | b' ')
        && b[13] == b':'
        && b[16] == b':';
    if !separators {
        return None;
    }
    let year = digits(text.get(0..4)?)?;
    let month = digits(text.get(5..7)?)?;
    let day = digits(text.get(8..10)?)?;
    let hour = digits(text.get(11..13)?)?;
    let minute = digits(text.get(14..16)?)?;
    let second = digits(text.get(17..19)?)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut rest = text.get(19..)?;
    let mut millis = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let len = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if len == 0 {
            return None;
        }
        let fraction_digits = fraction.as_bytes();
        for i in 0..3 {
            millis = millis * 10 + fraction_digits.get(i).filter(|_| i < len).map_or(0, |d| i64::from(d - b'0'));
        }
        rest = fraction.get(len..)?;
    }
    let offset_minutes = match rest {
        "" | "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let zone = rest.get(1..)?;
            let hours = digits(zone.get(0..2)?)?;
            let minutes = match zone.len() {
                2 => 0,
                4 => digits(zone.get(2..4)?)?,
                5 if zone.as_bytes()[2] == b':' => digits(zone.get(3..5)?)?,
                _ => return None,
            };
            sign * (hours * 60 + minutes)
        }
    };
    let days = days_from_civil(year, month, day);
    let minutes = (days * 24 + hour) * 60 + minute - offset_minutes;
    Some((minutes * 60 + second) * 1000 + millis)
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` for unix milliseconds.
pub(super) fn format_iso(ms: i64) -> String {
    let (year, month, day) = civil_from_days(ms.div_euclid(MS_PER_DAY));
    let of_day = ms.rem_euclid(MS_PER_DAY);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3_600_000,
        of_day / 60_000 % 60,
        of_day / 1000 % 60,
        of_day % 1000
    )
}

/// Parses a run of ASCII digits.
fn digits(text: &str) -> Option<i64> {
    if text.is_empty() || !text.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Proleptic Gregorian date of a day count since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_forms_omp_writes() {
        assert_eq!(parse_iso("2026-02-16T10:20:30.000Z"), Some(1_771_237_230_000));
        assert_eq!(parse_iso("2026-02-16T10:20:30Z"), Some(1_771_237_230_000));
        assert_eq!(parse_iso("2025-06-01T09:00:02.5Z"), Some(1_748_768_402_500));
        assert_eq!(parse_iso("2025-06-01T09:00:02.123456Z"), Some(1_748_768_402_123));
        assert_eq!(parse_iso("2025-06-01T11:00:05+02:00"), Some(1_748_768_405_000));
        assert_eq!(parse_iso("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_iso("1969-12-31T23:59:59.999Z"), Some(-1));
        assert_eq!(parse_iso("yesterday"), None);
        assert_eq!(parse_iso("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_iso("2026-01-01T00:00:00.Z"), None);
    }

    #[test]
    fn formats_with_millis() {
        assert_eq!(format_iso(1_771_237_230_000), "2026-02-16T10:20:30.000Z");
        assert_eq!(format_iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_iso(-1), "1969-12-31T23:59:59.999Z");
        for ms in [0, 951_782_400_123, 1_748_768_402_500, 4_102_444_799_999] {
            assert_eq!(parse_iso(&format_iso(ms)), Some(ms));
        }
    }
}
