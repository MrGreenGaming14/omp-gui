//! Shared styles for the panel tabs. Every color comes from the theme's
//! extended palette so dark and light themes apply.

use iced::widget::{button, container, rule, text};
use iced::{Background, Border, Color, Theme};

/// Height of one list row in pixels. Fixed so long lists can be virtualized
/// by arithmetic on the scroll offset.
pub const ROW_HEIGHT: f32 = 20.0;

/// Text size used in every row.
pub const TEXT_SIZE: f32 = 13.0;

/// Text size of list rows and the diff's file header, matching the sidebar.
pub const LIST_TEXT_SIZE: f32 = 14.0;

/// Height of one row in the files, session tree and agents lists, matching
/// the sidebar's rows.
pub const LIST_ROW_HEIGHT: f32 = 28.0;

/// Corner radius of row highlights.
const ROW_RADIUS: f32 = 6.0;

/// Pixels of indentation per tree level.
pub const INDENT: f32 = 14.0;

fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}

/// Secondary text: line numbers, counts, timestamps.
pub fn muted(theme: &Theme) -> text::Style {
    text::Style { color: Some(theme.palette().text.scale_alpha(0.55)) }
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
            (true, _) | (false, button::Status::Hovered | button::Status::Pressed) => {
                Some(Background::Color(palette.background.weak.color))
            }
            _ => None,
        };
        let text_color = palette.background.base.text;
        button::Style {
            background,
            text_color,
            border: Border { radius: ROW_RADIUS.into(), ..Border::default() },
            ..button::Style::default()
        }
    }
}

/// A small text button for actions inside a row (expand, cancel, fork):
/// muted text that brightens on a soft background when hovered.
pub fn action_button(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let (background, text_color) = match status {
        button::Status::Hovered | button::Status::Pressed => {
            (Some(Background::Color(palette.background.weak.color)), palette.background.base.text)
        }
        _ => (None, with_alpha(palette.background.base.text, 0.55)),
    };
    button::Style {
        background,
        text_color,
        border: Border { radius: 4.0.into(), ..Border::default() },
        ..button::Style::default()
    }
}

/// Background of an added line.
pub fn added_line(theme: &Theme) -> container::Style {
    container::Style::default().background(with_alpha(theme.extended_palette().success.base.color, 0.12))
}

/// Background of a removed line.
pub fn removed_line(theme: &Theme) -> container::Style {
    container::Style::default().background(with_alpha(theme.extended_palette().danger.base.color, 0.12))
}

/// Background of the changed part inside an added line.
pub fn added_emphasis(theme: &Theme) -> container::Style {
    container::Style::default().background(with_alpha(theme.extended_palette().success.base.color, 0.40))
}

/// Background of the changed part inside a removed line.
pub fn removed_emphasis(theme: &Theme) -> container::Style {
    container::Style::default().background(with_alpha(theme.extended_palette().danger.base.color, 0.36))
}

/// Background of hunk headers, folds and the empty side of a split row.
pub fn band(theme: &Theme) -> container::Style {
    container::Style::default().background(theme.extended_palette().background.weakest.color)
}

/// A 1px rule between the file header and the diff body.
pub fn divider(theme: &Theme) -> rule::Style {
    rule::Style { color: theme.extended_palette().background.weak.color, ..rule::default(theme) }
}

/// A small tag chip, like the host labels in the sidebar.
pub fn badge(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(theme.extended_palette().background.weak.color)),
        border: Border { radius: 4.0.into(), ..Border::default() },
        ..container::Style::default()
    }
}
