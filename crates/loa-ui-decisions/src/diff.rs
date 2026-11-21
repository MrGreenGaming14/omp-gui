//! Unified diff previews for approval cards.

use loa_ui_kit::theme::{SpaceRole as Sp, sp, TextRole as T, text_size as ts};
use loa_ui_kit::style::{self as kit, Surface, Tone};
use loa_ui_kit::theme::ColorRole as C;
use iced::widget::{column, container, text};
use iced::{Element, Fill, Font};

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


/// Renders `diff` in a monospace column on a sunken well. Added and removed
/// lines sit on a faint tint of the theme's success and danger colors.
pub fn view<'a, Message: 'a>(diff: &'a str) -> Element<'a, Message> {
    let lines = column(diff.lines().map(|line| {
        let (tone, tint) = match line_kind(line) {
            LineKind::Header | LineKind::Hunk => (Tone::Muted, None),
            LineKind::Added => (Tone::Ok, Some(C::AddedBg)),
            LineKind::Removed => (Tone::Danger, Some(C::RemovedBg)),
            LineKind::Context => (Tone::Normal, None),
        };
        let line = text(line).font(Font::MONOSPACE).size(ts(T::Label)).style(kit::text(tone));
        match tint {
            Some(role) => container(line).width(Fill).style(kit::fill(role, 0.0)).into(),
            None => line.into(),
        }
    }));
    container(lines)
        .padding([sp(Sp::Xs), sp(Sp::S)])
        .width(Fill)
        .style(kit::container(Surface::Well))
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
