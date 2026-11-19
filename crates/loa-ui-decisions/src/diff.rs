//! Unified diff previews for approval cards.

use crate::style::{muted, well};
use iced::widget::{column, container, text};
use iced::{Background, Color, Element, Fill, Font, Theme};

/// How one line of a unified diff renders.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// `---`, `+++`, `diff`, `index` lines.
    Header,
    /// `@@ … @@`.
    Hunk,
    Added,
    Removed,
    Context,
}

/// Classifies one line of a unified diff.
pub fn line_kind(line: &str) -> LineKind {
    if line.starts_with("+++")
        || line.starts_with("---")
        || line.starts_with("diff ")
        || line.starts_with("index ")
    {
        LineKind::Header
    } else if line.starts_with("@@") {
        LineKind::Hunk
    } else if line.starts_with('+') {
        LineKind::Added
    } else if line.starts_with('-') {
        LineKind::Removed
    } else {
        LineKind::Context
    }
}

/// Whether `body` is a unified diff rather than a command: it has a hunk
/// header, or a `---`/`+++` file header pair.
pub fn is_unified_diff(body: &str) -> bool {
    let mut old_header = false;
    for line in body.lines() {
        if line.starts_with("@@ ") {
            return true;
        }
        if line.starts_with("--- ") {
            old_header = true;
        } else if old_header && line.starts_with("+++ ") {
            return true;
        } else {
            old_header = false;
        }
    }
    false
}

type LineStyle = fn(&Theme) -> text::Style;
type Tint = fn(&Theme) -> Color;

/// Renders `diff` in a monospace column on a sunken well. Added and removed
/// lines sit on a faint tint of the theme's success and danger colors.
pub fn view<'a, Message: 'a>(diff: &'a str) -> Element<'a, Message> {
    let lines = column(diff.lines().map(|line| {
        let (style, tint): (LineStyle, Option<Tint>) = match line_kind(line) {
            LineKind::Header | LineKind::Hunk => (muted, None),
            LineKind::Added => (text::success, Some(added_tint)),
            LineKind::Removed => (text::danger, Some(removed_tint)),
            LineKind::Context => (text::default, None),
        };
        let line = text(line).font(Font::MONOSPACE).size(13).style(style);
        match tint {
            Some(tint) => container(line)
                .width(Fill)
                .style(move |theme: &Theme| container::Style {
                    background: Some(Background::Color(tint(theme))),
                    ..container::Style::default()
                })
                .into(),
            None => line.into(),
        }
    }));
    container(lines)
        .padding([5, 8])
        .width(Fill)
        .style(well)
        .into()
}

fn added_tint(theme: &Theme) -> Color {
    theme
        .extended_palette()
        .success
        .base
        .color
        .scale_alpha(0.07)
}

fn removed_tint(theme: &Theme) -> Color {
    theme.extended_palette().danger.base.color.scale_alpha(0.07)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDIT: &str = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    println!(\"hi\");\n+    println!(\"hello\");\n }\n";

    #[test]
    fn diff_lines_get_added_and_removed_colors() {
        let kinds: Vec<LineKind> = EDIT.lines().map(line_kind).collect();
        assert_eq!(
            kinds,
            [
                LineKind::Header,
                LineKind::Header,
                LineKind::Hunk,
                LineKind::Context,
                LineKind::Removed,
                LineKind::Added,
                LineKind::Context,
            ]
        );
    }

    #[test]
    fn commands_are_not_mistaken_for_diffs() {
        assert!(is_unified_diff(EDIT));
        assert!(is_unified_diff("@@ -0,0 +1 @@\n+new file\n"));
        assert!(!is_unified_diff("rm -rf --no-preserve-root ./build"));
        assert!(!is_unified_diff("git diff --stat\n--- not a header pair"));
    }
}
