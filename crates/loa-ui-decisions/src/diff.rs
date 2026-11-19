//! Unified diff previews for approval cards.

use iced::widget::{column, text};
use iced::{Element, Font};

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

/// Renders `diff` in a monospace column, added and removed lines in the
/// theme's success and danger colors.
pub fn view<'a, Message: 'a>(diff: &'a str) -> Element<'a, Message> {
    column(diff.lines().map(|line| {
        let style = match line_kind(line) {
            LineKind::Header => text::secondary,
            LineKind::Hunk => text::primary,
            LineKind::Added => text::success,
            LineKind::Removed => text::danger,
            LineKind::Context => text::default,
        };
        text(line).font(Font::MONOSPACE).style(style).into()
    }))
    .into()
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
