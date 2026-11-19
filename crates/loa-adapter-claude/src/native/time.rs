//! ISO 8601 UTC timestamps as Claude Code writes them
//! (`2026-01-02T03:04:05.678Z`), to and from unix milliseconds.

const MS_PER_DAY: i64 = 86_400_000;

/// Parses `YYYY-MM-DDTHH:MM:SS[.fraction](Z|+00:00)`. Digits past
/// milliseconds are dropped.
pub(super) fn parse(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' || bytes[13] != b':' || bytes[16] != b':'
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let digits = text.get(from..to)?;
        digits.bytes().all(|b| b.is_ascii_digit()).then(|| digits.parse().ok())?
    };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if civil_from_days(days) != (year, month, day) {
        return None;
    }
    let mut rest = text.get(19..)?;
    let mut millis = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let mut scale = 100;
        for b in fraction.bytes().take(digits.min(3)) {
            millis += i64::from(b - b'0') * scale;
            scale /= 10;
        }
        rest = &fraction[digits..];
    }
    if rest != "Z" && rest != "+00:00" {
        return None;
    }
    Some(days * MS_PER_DAY + ((hour * 60 + minute) * 60 + second) * 1000 + millis)
}

/// Formats as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub(super) fn format(ms: i64) -> String {
    let (year, month, day) = civil_from_days(ms.div_euclid(MS_PER_DAY));
    let rem = ms.rem_euclid(MS_PER_DAY);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    )
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date `days` after 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats_claude_timestamps() {
        assert_eq!(parse("2026-01-02T03:04:05.678Z"), Some(1_767_323_045_678));
        assert_eq!(format(1_767_323_045_678), "2026-01-02T03:04:05.678Z");
        assert_eq!(parse("2024-02-29T00:00:00Z"), Some(1_709_164_800_000));
        assert_eq!(parse("2023-02-29T00:00:00.000Z"), None);
        assert_eq!(parse("1969-12-31T23:59:59.999Z"), Some(-1));
        assert_eq!(format(-1), "1969-12-31T23:59:59.999Z");
    }
}
