//! Shared styles for the panel tabs. Every color comes from the theme's
//! extended palette so dark and light themes apply.

use iced::widget::{button, container, text};
use iced::{Background, Border, Color, Theme};

/// Height of one list row in pixels. Fixed so long lists can be virtualized
/// by arithmetic on the scroll offset.
pub const ROW_HEIGHT: f32 = 20.0;

/// Text size used in every row.
pub const TEXT_SIZE: f32 = 13.0;

/// Pixels of indentation per tree level.
pub const INDENT: f32 = 14.0;

fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}

/// Secondary text: line numbers, counts, timestamps.
pub fn muted(theme: &Theme) -> text::Style {
    text::Style { color: Some(theme.extended_palette().background.strong.color) }
}

/// Text in the danger color: errors, removed counts.
pub fn danger_text(theme: &Theme) -> text::Style {
    text::Style { color: Some(theme.extended_palette().danger.base.color) }
}

/// Text in the success color: added counts, finished jobs.
pub fn success_text(theme: &Theme) -> text::Style {
    text::Style { color: Some(theme.extended_palette().success.base.color) }
}

/// Text in the primary color: markers and links.
pub fn primary_text(theme: &Theme) -> text::Style {
    text::Style { color: Some(theme.extended_palette().primary.base.color) }
}

/// A row button that looks like plain text until hovered or selected.
pub fn row_button(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let palette = theme.extended_palette();
        let background = match (selected, status) {
            (true, _) => Some(Background::Color(palette.primary.weak.color)),
            (false, button::Status::Hovered | button::Status::Pressed) => {
                Some(Background::Color(palette.background.weak.color))
            }
            _ => None,
        };
        let text_color = if selected { palette.primary.weak.text } else { palette.background.base.text };
        button::Style { background, text_color, border: Border::default(), ..button::Style::default() }
    }
}

/// A small text button for actions inside a row (expand, cancel, fork).
pub fn action_button(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Some(Background::Color(palette.background.strong.color)),
        _ => Some(Background::Color(palette.background.weak.color)),
    };
    button::Style {
        background,
        text_color: palette.background.base.text,
        border: Border { radius: 3.0.into(), ..Border::default() },
        ..button::Style::default()
    }
}

/// Background of an added line.
pub fn added_line(theme: &Theme) -> container::Style {
    container::Style::default().background(with_alpha(theme.extended_palette().success.base.color, 0.15))
}

/// Background of a removed line.
pub fn removed_line(theme: &Theme) -> container::Style {
    container::Style::default().background(with_alpha(theme.extended_palette().danger.base.color, 0.15))
}

/// Background of the changed part inside an added line.
pub fn added_emphasis(theme: &Theme) -> container::Style {
    container::Style::default().background(with_alpha(theme.extended_palette().success.base.color, 0.40))
}

/// Background of the changed part inside a removed line.
pub fn removed_emphasis(theme: &Theme) -> container::Style {
    container::Style::default().background(with_alpha(theme.extended_palette().danger.base.color, 0.40))
}

/// Background of hunk headers, folds and the empty side of a split row.
pub fn band(theme: &Theme) -> container::Style {
    container::Style::default().background(theme.extended_palette().background.weak.color)
}
