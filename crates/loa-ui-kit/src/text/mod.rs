//! Text helpers for views: fuzzy filtering and highlighting, durations, sizes and truncation.

pub mod ellipsis;
pub mod fuzzy;

pub use ellipsis::{ELLIPSIS, EllipsisMode, ellipsize_text, ellipsize_with, measure_text_width};
pub use fuzzy::{Class, Match, Matcher, highlight_runs};

/// Indices of `candidates` matching `query`, best first (class, score, shorter, earlier), at
/// most `limit`. An empty query keeps the original order.
pub fn filter<'a>(query: &str, candidates: impl IntoIterator<Item = &'a str>, limit: usize) -> Vec<usize> {
    let mut m = Matcher::new();
    m.set_query(query);
    let mut hits: Vec<_> = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(i, c)| m.matches(c).map(|h| (h.class, std::cmp::Reverse(h.score), if m.is_empty() { 0 } else { c.len() }, i)))
        .collect();
    hits.sort_unstable();
    hits.into_iter().take(limit).map(|h| h.3).collect()
}

/// "0.4s", "12s", "2m 5s", "1h 3m".
pub fn format_duration(ms: i64) -> String {
    match ms {
        ..1_000 => format!("{:.1}s", ms as f64 / 1000.0),
        1_000..60_000 => format!("{}s", ms / 1000),
        60_000..3_600_000 => format!("{}m {}s", ms / 60_000, ms % 60_000 / 1000),
        _ => format!("{}h {}m", ms / 3_600_000, ms % 3_600_000 / 60_000),
    }
}

/// "512 B", "1.5 KB", "3.0 MB", "2.1 GB".
pub fn format_bytes(bytes: u64) -> String {
    let mut v = bytes as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if v < 1024.0 || unit == "GB" {
            return if unit == "B" { format!("{bytes} B") } else { format!("{v:.1} {unit}") };
        }
        v /= 1024.0;
    }
    unreachable!()
}

/// `s` cut to at most `max` chars, ending in `…` when cut.
pub fn truncate_trailing(s: &str, max: usize) -> String {
    match s.char_indices().nth(max.saturating_sub(1)) {
        Some((cut, _)) if s.chars().count() > max => format!("{}…", &s[..cut]),
        _ => s.to_owned(),
    }
}

/// `s` cut to its last `max` chars, starting with `…` when cut (paths keep their file name).
pub fn truncate_leading(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max { return s.to_owned() }
    let skip = s.char_indices().nth(n + 1 - max.max(1)).map_or(s.len(), |(i, _)| i);
    format!("…{}", &s[skip..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_truncates() {
        let d = [(400, "0.4s"), (12_000, "12s"), (125_000, "2m 5s"), (3_780_000, "1h 3m")];
        d.iter().for_each(|&(ms, s)| assert_eq!(format_duration(ms), s));
        let b = [(512, "512 B"), (1536, "1.5 KB"), (3 << 20, "3.0 MB"), (5 << 40, "5120.0 GB")];
        b.iter().for_each(|&(n, s)| assert_eq!(format_bytes(n), s));
        assert_eq!((truncate_trailing("abcdef", 4), truncate_trailing("ab", 4)), ("abc…".into(), "ab".into()));
        assert_eq!((truncate_leading("src/main.rs", 7), truncate_leading("ab", 4)), ("…ain.rs".into(), "ab".into()));
        assert_eq!(filter("main", ["crates/composer/src/model.rs", "src/main.rs"], 10), [1]);
        assert_eq!(filter("", ["b", "a", "c"], 2), [0, 1]);
    }
}
