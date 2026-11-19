//! Looks of the composer. Colors come from the theme's extended palette, so
//! the light and dark themes both work.

use iced::widget::{button, container, svg, text, text_editor};
use iced::{Background, Border, Color, Theme};

/// Corner radius of the composer box.
const FRAME_RADIUS: f32 = 10.0;
/// Corner radius of menus.
const MENU_RADIUS: f32 = 8.0;
/// Corner radius of rows, pickers and the send button.
const RADIUS: f32 = 6.0;

/// Secondary text: the body text color at reduced alpha.
pub(crate) fn muted_color(theme: &Theme) -> Color {
    theme.palette().text.scale_alpha(0.55)
}

pub(crate) fn muted(theme: &Theme) -> text::Style {
    text::Style { color: Some(muted_color(theme)) }
}

/// One-pixel edge of the composer box and menus.
fn hairline(theme: &Theme, radius: f32) -> Border {
    Border { color: theme.extended_palette().background.weak.color, width: 1.0, radius: radius.into() }
}

/// The composer box: the page background inside a hairline edge.
pub(crate) fn frame(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(theme.palette().background)),
        border: hairline(theme, FRAME_RADIUS),
        ..container::Style::default()
    }
}

/// The editor inside the box: no fill and no edge of its own.
pub(crate) fn editor(theme: &Theme, _status: text_editor::Status) -> text_editor::Style {
    let palette = theme.extended_palette();
    text_editor::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        placeholder: muted_color(theme),
        value: palette.background.base.text,
        selection: palette.primary.weak.color.scale_alpha(0.4),
    }
}

/// Raised surface of the popups and picker menus, like the command palette.
pub(crate) fn menu(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(theme.extended_palette().background.weakest.color)),
        border: hairline(theme, MENU_RADIUS),
        ..container::Style::default()
    }
}

/// A menu row. `selected` is the highlighted row; others light up on hover.
/// Disabled rows (unavailable choices) keep no fill and dim their text.
pub(crate) fn row(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme: &Theme, status: button::Status| {
        let palette = theme.extended_palette();
        let fill = match status {
            _ if selected => Some(palette.background.neutral.color),
            button::Status::Hovered | button::Status::Pressed => Some(palette.background.weak.color),
            button::Status::Active | button::Status::Disabled => None,
        };
        let text_color = match status {
            button::Status::Disabled => muted_color(theme),
            _ => palette.background.base.text,
        };
        button::Style {
            background: fill.map(Background::Color),
            text_color,
            border: Border { radius: RADIUS.into(), ..Border::default() },
            ..button::Style::default()
        }
    }
}

/// A flat picker: just its label and chevron, filled on hover or while its
/// menu is open.
pub(crate) fn picker(open: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme: &Theme, status: button::Status| {
        let palette = theme.extended_palette();
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: (open || hovered).then_some(Background::Color(palette.background.weak.color)),
            text_color: palette.background.base.text,
            border: Border { radius: RADIUS.into(), ..Border::default() },
            ..button::Style::default()
        }
    }
}

/// The square send and stop button: filled with the primary color, slightly
/// dimmed while there is nothing to send.
pub(crate) fn send(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let fill = match status {
        button::Status::Hovered | button::Status::Pressed => palette.primary.strong.color,
        button::Status::Disabled => palette.primary.base.color.scale_alpha(0.85),
        button::Status::Active => palette.primary.base.color,
    };
    button::Style {
        background: Some(Background::Color(fill)),
        text_color: palette.primary.base.text,
        border: Border { radius: RADIUS.into(), ..Border::default() },
        ..button::Style::default()
    }
}

/// Icon tint on the send button: the text color of the primary fill.
pub(crate) fn on_primary(theme: &Theme, _status: svg::Status) -> svg::Style {
    svg::Style { color: Some(theme.extended_palette().primary.base.text) }
}
