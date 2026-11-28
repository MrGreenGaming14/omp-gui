//! Font-measurement-based single-line ellipsis helpers with [`EllipsisMode::End`] and
//! [`EllipsisMode::Path`] truncation.

use iced::Font;

/// The single-character ellipsis appended or inserted when text is truncated.
pub const ELLIPSIS: &str = "…";

/// How overflowing single-line text is shortened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EllipsisMode {
    /// Keep as much of the start as fits and append `…`.
    End,
    /// Preserve the last path component (supporting `/` and `\`), eliding parent segments.
    Path,
}

/// Measures the single-line rendered width of `content` at `size` logical pixels in `font`.
pub fn measure_text_width(content: &str, size: f32, font: Font) -> f32 {
    use iced::advanced::graphics::text::Paragraph;
    use iced::advanced::text::{self, Paragraph as _};
    use iced::{Pixels, Size, alignment};

    if content.is_empty() || size <= 0.0 || !size.is_finite() {
        return 0.0;
    }
    let shaping = if content.is_ascii() { text::Shaping::Basic } else { text::Shaping::Advanced };
    let paragraph = Paragraph::with_text(text::Text {
        content,
        bounds: Size::INFINITE,
        size: Pixels(size),
        line_height: text::LineHeight::default(),
        font,
        align_x: text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping,
        wrapping: text::Wrapping::None,
    });
    paragraph.min_bounds().width
}

/// Shortens `content` to fit within `max_width` using real font measurement.
pub fn ellipsize_text(content: &str, max_width: f32, size: f32, font: Font, mode: EllipsisMode) -> String {
    ellipsize_with(content, max_width, mode, |candidate| measure_text_width(candidate, size, font))
}

/// Shortens `text` to fit within `max_width` using a caller-supplied width `measure` closure.
pub fn ellipsize_with(
    text: &str,
    max_width: f32,
    mode: EllipsisMode,
    mut measure: impl FnMut(&str) -> f32,
) -> String {
    if text.is_empty() || max_width <= 0.0 || max_width.is_nan() {
        return String::new();
    }
    if max_width == f32::INFINITY || measure(text) <= max_width {
        return text.to_owned();
    }
    match mode {
        EllipsisMode::End => ellipsize_end(text, max_width, &mut measure),
        EllipsisMode::Path => ellipsize_path(text, max_width, &mut measure),
    }
}

fn ellipsize_end(text: &str, max_width: f32, measure: &mut impl FnMut(&str) -> f32) -> String {
    if text.is_empty() || max_width <= 0.0 || max_width.is_nan() {
        return String::new();
    }
    if max_width == f32::INFINITY || measure(text) <= max_width {
        return text.to_owned();
    }
    if measure(ELLIPSIS) > max_width {
        return String::new();
    }
    let boundaries: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let mut lo = 0usize;
    let mut hi = boundaries.len();
    let mut buf = String::with_capacity(text.len() + ELLIPSIS.len());
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        buf.clear();
        buf.push_str(&text[..boundaries[mid]]);
        buf.push_str(ELLIPSIS);
        if measure(&buf) <= max_width {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    buf.clear();
    buf.push_str(&text[..boundaries[lo]]);
    buf.push_str(ELLIPSIS);
    buf
}

fn ellipsize_path(text: &str, max_width: f32, measure: &mut impl FnMut(&str) -> f32) -> String {
    let trimmed = text.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return ellipsize_end(text, max_width, measure);
    }
    if trimmed.len() < text.len() && measure(trimmed) <= max_width {
        return trimmed.to_owned();
    }
    let Some(sep_idx) = trimmed.rfind(['/', '\\']).filter(|&idx| idx > 0) else {
        return ellipsize_end(text, max_width, measure);
    };
    let parent = &trimmed[..sep_idx];
    let sep = &trimmed[sep_idx..sep_idx + 1];
    let tail = &trimmed[sep_idx + 1..];
    let suffix = format!("{ELLIPSIS}{sep}{tail}");
    if measure(&suffix) > max_width {
        return ellipsize_end(tail, max_width, measure);
    }

    let boundaries: Vec<usize> = parent.char_indices().map(|(i, _)| i).collect();
    let mut lo = 0usize;
    let mut hi = boundaries.len();
    let mut buf = String::with_capacity(parent.len() + suffix.len());
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        buf.clear();
        buf.push_str(&parent[..boundaries[mid]]);
        buf.push_str(&suffix);
        if measure(&buf) <= max_width {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    buf.clear();
    buf.push_str(&parent[..boundaries[lo]]);
    buf.push_str(&suffix);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono(s: &str) -> f32 {
        s.chars().count() as f32 * 10.0
    }

    fn weighted(s: &str) -> f32 {
        s.chars()
            .map(|c| match c {
                'i' | 'l' | '.' | '/' | '\\' => 4.0,
                'W' | 'M' => 14.0,
                '…' => 10.0,
                c if c.len_utf8() > 1 => 20.0,
                _ => 8.0,
            })
            .sum()
    }

    #[test]
    fn fits_without_truncation_in_both_modes() {
        let s = "crates/loa-ui-kit/src/text/mod.rs";
        assert_eq!(ellipsize_with(s, mono(s), EllipsisMode::End, mono), s);
        assert_eq!(ellipsize_with(s, mono(s), EllipsisMode::Path, mono), s);
        assert_eq!(ellipsize_with(s, f32::INFINITY, EllipsisMode::End, mono), s);
        assert_eq!(ellipsize_with("", 100.0, EllipsisMode::Path, mono), "");
    }

    #[test]
    fn end_mode_respects_measured_budget() {
        assert_eq!(ellipsize_with("abcdef", 40.0, EllipsisMode::End, mono), "abc…");
        let variable = "WiiiiiMMM";
        let out = ellipsize_with(variable, 40.0, EllipsisMode::End, weighted);
        assert!(out.ends_with(ELLIPSIS));
        assert!(weighted(&out) <= 40.0, "{out} exceeded budget");
        assert_eq!(out, "Wiiii…");
    }

    #[test]
    fn path_mode_preserves_last_component_for_both_separators() {
        let unix = "/Users/alice/projects/loa-ui-kit";
        assert_eq!(ellipsize_with(unix, 130.0, EllipsisMode::Path, mono), "/…/loa-ui-kit");
        assert_eq!(ellipsize_with(unix, 200.0, EllipsisMode::Path, mono), "/Users/a…/loa-ui-kit");

        let windows = r"C:\Users\alice\projects\loa-ui-kit";
        assert_eq!(ellipsize_with(windows, 130.0, EllipsisMode::Path, mono), r"C…\loa-ui-kit");
        assert_eq!(ellipsize_with(windows, 170.0, EllipsisMode::Path, mono), r"C:\Us…\loa-ui-kit");

        let mixed = r"C:\Users/alice\projects/my-repo";
        assert_eq!(ellipsize_with(mixed, 110.0, EllipsisMode::Path, mono), "C:…/my-repo");

        let trailing = "/alpha/beta/gamma/";
        assert_eq!(ellipsize_with(trailing, 80.0, EllipsisMode::Path, mono), "/…/gamma");

        // When even "…/<tail>" is wider than max_width, falls back to the tail component.
        assert_eq!(ellipsize_with("/a/b/long_component", 100.0, EllipsisMode::Path, mono), "long_comp…");
        assert_eq!(ellipsize_with("single_segment_name", 80.0, EllipsisMode::Path, mono), "single_…");
    }

    #[test]
    fn tiny_and_invalid_widths_never_overflow_or_panic() {
        for width in [0.0, -5.0, f32::NAN, 4.0, 9.9] {
            assert_eq!(ellipsize_with("hello/world", width, EllipsisMode::End, mono), "");
            assert_eq!(ellipsize_with("hello/world", width, EllipsisMode::Path, mono), "");
        }
        assert_eq!(ellipsize_with("hello", 10.0, EllipsisMode::End, mono), ELLIPSIS);
        assert_eq!(ellipsize_with("a/very_long_tail", 10.0, EllipsisMode::Path, mono), ELLIPSIS);
    }

    #[test]
    fn multibyte_unicode_is_safe() {
        let title = "café 🚀 日本語プロジェクト";
        for width in [0.0, 10.0, 35.0, 60.0, 120.0, 500.0] {
            let out = ellipsize_with(title, width, EllipsisMode::End, weighted);
            assert!(weighted(&out) <= width, "width {width}: {out}");
        }
        let path = r"/home/ünicode/日本語/résumé_📄.rs";
        let kept = ellipsize_with(path, 220.0, EllipsisMode::Path, weighted);
        assert!(kept.ends_with("/résumé_📄.rs"), "expected last component preserved, got {kept}");
        assert!(kept.contains(ELLIPSIS));
        assert!(weighted(&kept) <= 220.0);

        let tight = ellipsize_with(path, 90.0, EllipsisMode::Path, weighted);
        assert!(weighted(&tight) <= 90.0);
    }

    #[test]
    fn real_font_measurement_fits_and_ellipsizes() {
        let s = "A reasonably long session title for narrow windows";
        let full = measure_text_width(s, 14.0, Font::DEFAULT);
        assert!(full > 100.0);
        assert_eq!(ellipsize_text(s, full + 1.0, 14.0, Font::DEFAULT, EllipsisMode::End), s);

        let cut = ellipsize_text(s, full * 0.4, 14.0, Font::DEFAULT, EllipsisMode::End);
        assert!(cut.ends_with(ELLIPSIS));
        assert_ne!(cut, s);
        assert!(measure_text_width(&cut, 14.0, Font::DEFAULT) <= full * 0.4 + 0.5);

        let path = "~/.omp/agent/OMGUI-worktrees/issue-331-narrow-overflow";
        let path_w = measure_text_width(path, 13.0, Font::DEFAULT);
        let tail_w = measure_text_width("…/issue-331-narrow-overflow", 13.0, Font::DEFAULT);
        let budget = tail_w + 24.0;
        assert!(budget < path_w);
        let cut_path = ellipsize_text(path, budget, 13.0, Font::DEFAULT, EllipsisMode::Path);
        assert!(cut_path.ends_with("/issue-331-narrow-overflow"));
        assert!(cut_path.contains(ELLIPSIS));
        assert!(measure_text_width(&cut_path, 13.0, Font::DEFAULT) <= budget + 0.5);
    }
}
