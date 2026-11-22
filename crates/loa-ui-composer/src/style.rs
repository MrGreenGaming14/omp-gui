//! Looks of the composer. Colors come from the shell's theme tokens, so the
//! light and dark themes both work.

use iced::widget::{button, container, slider, svg, text, text_editor};
use iced::{Background, Border, Color, Theme};
use loa_ui_shell::theme::style::tokens;

/// Corner radius of the composer box.
const FRAME_RADIUS: f32 = 16.0;
/// Corner radius of menus.
const MENU_RADIUS: f32 = 8.0;
/// Corner radius of rows and the ghost controls.
const RADIUS: f32 = 6.0;

/// Secondary text: labels of the ghost controls, placeholders.
pub(crate) fn muted_color(theme: &Theme) -> Color {
    tokens(theme).text2
}

pub(crate) fn muted(theme: &Theme) -> text::Style {
    text::Style { color: Some(muted_color(theme)) }
}

/// Tertiary text, e.g. the folder chip's branch.
pub(crate) fn faint(theme: &Theme) -> text::Style {
    text::Style { color: Some(tokens(theme).text3) }
}

/// One-pixel edge of the composer box and menus.
fn hairline(theme: &Theme, radius: f32) -> Border {
    Border { color: tokens(theme).line, width: 1.0, radius: radius.into() }
}

/// The composer box: a raised fill a step above the ground, inside a
/// hairline edge.
pub(crate) fn frame(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(Background::Color(t.raised)),
        text_color: Some(t.text),
        border: Border { color: t.line_strong.scale_alpha(0.6), width: 1.0, radius: FRAME_RADIUS.into() },
        ..container::Style::default()
    }
}

/// The editor inside the box: no fill and no edge of its own.
pub(crate) fn editor(theme: &Theme, _status: text_editor::Status) -> text_editor::Style {
    let t = tokens(theme);
    text_editor::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        placeholder: t.text3,
        value: t.text,
        selection: t.accent.scale_alpha(0.3),
    }
}

/// Raised surface of the popups and picker menus, like the command palette.
pub(crate) fn menu(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(Background::Color(t.card)),
        text_color: Some(t.text),
        border: hairline(theme, MENU_RADIUS),
        ..container::Style::default()
    }
}

/// The effort card: the menu surface with rounder corners.
pub(crate) fn effort_card(theme: &Theme) -> container::Style {
    container::Style { border: hairline(theme, 14.0), ..menu(theme) }
}

/// The current effort level in the card's heading.
pub(crate) fn accent_text(theme: &Theme) -> text::Style {
    text::Style { color: Some(tokens(theme).accent) }
}

/// The effort slider: a thick rounded track, filled with the accent up to a
/// large white knob.
pub(crate) fn effort_slider(rail: f32, knob: f32) -> impl Fn(&Theme, slider::Status) -> slider::Style {
    move |theme, _status| {
        let t = tokens(theme);
        slider::Style {
            rail: slider::Rail {
                backgrounds: (Background::Color(t.accent), Background::Color(t.raised)),
                width: rail,
                border: Border { color: t.line, width: 1.0, radius: (rail / 2.0).into() },
            },
            handle: slider::Handle {
                shape: slider::HandleShape::Circle { radius: knob },
                background: Background::Color(Color::WHITE),
                border_width: 0.0,
                border_color: Color::TRANSPARENT,
            },
        }
    }
}

/// A level mark on the effort track: light on the filled part, faint on the rest.
pub(crate) fn effort_dot(filled: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = tokens(theme);
        let color = if filled { Color::WHITE.scale_alpha(0.6) } else { t.text3 };
        container::Style { background: Some(Background::Color(color)), border: Border { radius: 99.0.into(), ..Border::default() }, ..container::Style::default() }
    }
}

/// A menu row. `selected` is the highlighted row; others light up on hover.
/// Disabled rows (unavailable choices) keep no fill and dim their text.
pub(crate) fn row(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme: &Theme, status: button::Status| {
        let t = tokens(theme);
        let fill = match status {
            _ if selected => Some(t.selected),
            button::Status::Hovered | button::Status::Pressed => Some(t.selected),
            button::Status::Active | button::Status::Disabled => None,
        };
        let text_color = match status {
            button::Status::Disabled => t.text3,
            _ => t.text,
        };
        button::Style {
            background: fill.map(Background::Color),
            text_color,
            border: Border { radius: RADIUS.into(), ..Border::default() },
            ..button::Style::default()
        }
    }
}

/// A borderless ghost control (pickers, `+`, the folder chip): muted label,
/// a soft fill only on hover or while its menu is open.
pub(crate) fn ghost(open: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme: &Theme, status: button::Status| {
        let t = tokens(theme);
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: (open || hovered).then_some(Background::Color(t.hover)),
            text_color: if matches!(status, button::Status::Disabled) {
                t.text3
            } else if open || hovered {
                t.text
            } else {
                t.text2
            },
            border: Border { radius: RADIUS.into(), ..Border::default() },
            ..button::Style::default()
        }
    }
}

/// Icon tint of the ghost controls: secondary text.
pub(crate) fn ghost_icon(theme: &Theme, _status: svg::Status) -> svg::Style {
    svg::Style { color: Some(tokens(theme).text2) }
}

/// Tint of the ghost controls' chevrons: a step quieter than their icons.
pub(crate) fn chevron(theme: &Theme, _status: svg::Status) -> svg::Style {
    svg::Style { color: Some(tokens(theme).text3) }
}

/// The round send and stop button: the accent while there is something to
/// send (or a turn to stop), a quiet neutral disc otherwise.
pub(crate) fn send(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let fill = match status {
        button::Status::Hovered | button::Status::Pressed => t.accent.scale_alpha(0.85),
        button::Status::Disabled => t.selected,
        button::Status::Active => t.accent,
    };
    button::Style {
        background: Some(Background::Color(fill)),
        text_color: t.on_accent,
        border: Border { radius: (crate::SEND_SIZE / 2.0).into(), ..Border::default() },
        ..button::Style::default()
    }
}

/// Icon tint on the send button: the text color on the accent.
pub(crate) fn on_accent(theme: &Theme, _status: svg::Status) -> svg::Style {
    svg::Style { color: Some(tokens(theme).on_accent) }
}

/// Icon tint on the disabled send button.
pub(crate) fn send_idle(theme: &Theme, _status: svg::Status) -> svg::Style {
    svg::Style { color: Some(tokens(theme).text3) }
}
