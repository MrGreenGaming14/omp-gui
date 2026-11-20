//! Looks of the terminal tab bar, derived from the theme's extended palette.

use iced::widget::{button, rule, text};
use iced::{Background, Color, Theme, border};

/// Secondary text color: about 60% of the text color.
fn muted_color(theme: &Theme) -> Color {
    theme.extended_palette().background.base.text.scale_alpha(0.6)
}

/// Secondary text: inactive tabs and status lines.
pub fn muted(theme: &Theme) -> text::Style {
    text::Style { color: Some(muted_color(theme)) }
}

/// A sub tab: raised when active, flat and muted otherwise.
pub fn tab(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let palette = theme.extended_palette();
        let background = match (active, status) {
            (true, _) | (false, button::Status::Hovered | button::Status::Pressed) => {
                Some(Background::Color(palette.background.weak.color))
            }
            (false, _) => None,
        };
        button::Style {
            background,
            text_color: if active { palette.background.base.text } else { muted_color(theme) },
            border: border::rounded(6),
            ..button::Style::default()
        }
    }
}

/// An icon button with no fill until hovered.
pub fn icon(theme: &Theme, status: button::Status) -> button::Style {
    let background = matches!(status, button::Status::Hovered | button::Status::Pressed)
        .then(|| Background::Color(theme.extended_palette().background.weak.color));
    button::Style { background, border: border::rounded(4), ..button::Style::default() }
}

/// The 1px line under the tab bar.
pub fn divider<'a>() -> rule::Rule<'a> {
    rule::horizontal(1).style(|theme: &Theme| rule::Style {
        color: theme.extended_palette().background.weak.color,
        radius: 0.0.into(),
        fill_mode: rule::FillMode::Full,
        snap: true,
    })
}

/// A bordered button: the empty tab's "Open a terminal".
pub fn secondary(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let background = matches!(status, button::Status::Hovered | button::Status::Pressed)
        .then(|| Background::Color(palette.background.weak.color));
    button::Style {
        background,
        text_color: palette.background.base.text,
        border: iced::Border { color: palette.background.strong.color, width: 1.0, radius: 6.0.into() },
        ..button::Style::default()
    }
}
