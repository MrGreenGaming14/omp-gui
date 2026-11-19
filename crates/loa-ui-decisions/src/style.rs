//! Looks shared by the decision cards. Colors come from the theme's extended
//! palette, so light and dark themes both work.

use iced::widget::{button, container, pick_list, text, text_editor, text_input};
use iced::{Background, Border, Color, Theme};

/// Corner radius shared by cards, previews, fields and buttons.
const RADIUS: f32 = 6.0;

/// Secondary text: the body text color at reduced alpha.
pub(crate) fn muted_color(theme: &Theme) -> Color {
    theme.palette().text.scale_alpha(0.55)
}

/// Text style for secondary lines.
pub(crate) fn muted(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(muted_color(theme)),
    }
}

/// Focused edge: one step brighter than the resting edge (`neutral`, about
/// #242424 on the dark palette; the mockup's card edge samples near #222222).
fn focus_color(theme: &Theme) -> Color {
    theme.extended_palette().background.neutral.color
}

/// One-pixel edge; it brightens while the owner has focus.
fn hairline(theme: &Theme, focused: bool) -> Border {
    Border {
        color: if focused {
            focus_color(theme)
        } else {
            theme.extended_palette().background.weak.color
        },
        width: 1.0,
        radius: RADIUS.into(),
    }
}

/// Card frame: a hairline border that brightens while the card has focus.
pub(crate) fn card(focused: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme: &Theme| container::Style {
        border: hairline(theme, focused),
        ..container::Style::default()
    }
}

/// Raised surface for command previews.
pub(crate) fn raised(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(
            theme.extended_palette().background.weak.color,
        )),
        border: Border {
            radius: RADIUS.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// Sunken surface for diff previews, one step darker than [`raised`].
pub(crate) fn well(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(
            theme.extended_palette().background.weakest.color,
        )),
        border: Border {
            radius: RADIUS.into(),
            ..Border::default()
        },
        ..container::Style::default()
    }
}

/// Button look. `primary` fills the button with the primary color; otherwise
/// it is outlined. `selected` marks the option a number key highlighted.
pub(crate) fn action(
    primary: bool,
    selected: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme: &Theme, status: button::Status| {
        let palette = theme.extended_palette();
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let dim = if matches!(status, button::Status::Disabled) {
            0.5
        } else {
            1.0
        };
        let (fill, text_color, border_color) = if primary {
            let fill = if hovered {
                palette.primary.strong.color
            } else {
                palette.primary.base.color
            };
            (Some(fill), palette.primary.base.text, fill)
        } else {
            let border = if selected {
                palette.primary.base.color
            } else {
                palette.background.weak.color
            };
            let fill = hovered.then_some(palette.background.weak.color);
            (fill, palette.background.base.text, border)
        };
        button::Style {
            background: fill.map(|color| Background::Color(color.scale_alpha(dim))),
            text_color: text_color.scale_alpha(dim),
            border: Border {
                color: border_color.scale_alpha(dim),
                width: 1.0,
                radius: RADIUS.into(),
            },
            ..button::Style::default()
        }
    }
}

/// Single-line input: near-black fill, hairline edge, muted placeholder.
pub(crate) fn field(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    let mut style = text_input::default(theme, status);
    style.background = Background::Color(theme.extended_palette().background.weakest.color);
    style.border = hairline(theme, focused);
    style.placeholder = muted_color(theme);
    style
}

/// Multi-line editor with the same surface as [`field`].
pub(crate) fn editor(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let focused = matches!(status, text_editor::Status::Focused { .. });
    let mut style = text_editor::default(theme, status);
    style.background = Background::Color(theme.extended_palette().background.weakest.color);
    style.border = hairline(theme, focused);
    style.placeholder = muted_color(theme);
    style
}

/// Dropdown with the same surface as [`field`].
pub(crate) fn select(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let focused = matches!(status, pick_list::Status::Opened { .. });
    let mut style = pick_list::default(theme, status);
    style.background = Background::Color(theme.extended_palette().background.weakest.color);
    style.border = hairline(theme, focused);
    style
}
