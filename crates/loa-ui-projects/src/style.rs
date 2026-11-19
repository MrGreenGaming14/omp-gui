//! Looks of the project screen, derived from the theme's extended palette.
//!
//! The app's shared looks live in `loa-ui-shell`, which this crate cannot
//! depend on, so the ones the screen needs are mirrored here.

use iced::widget::{button, container, rule, scrollable, text, text_editor, text_input};
use iced::{Background, Border, Color, Theme, border};

/// Corner radius of cards, fields and buttons.
const RADIUS: f32 = 6.0;
/// Corner radius of small pills.
const PILL_RADIUS: f32 = 4.0;

/// The project name.
pub const TITLE: f32 = 20.0;
/// Primary text: names and field values.
pub const BODY: f32 = 14.0;
/// Secondary text: details, hints, buttons.
pub const SMALL: f32 = 12.0;
/// Digest summaries and empty states.
pub const NOTE: f32 = 13.0;

/// Secondary text color.
pub fn muted_color(theme: &Theme) -> Color {
    theme.extended_palette().background.base.text.scale_alpha(0.55)
}

/// Secondary text: details, hints, digest summaries.
pub fn muted(theme: &Theme) -> text::Style {
    text::Style { color: Some(muted_color(theme)) }
}

/// Text in the danger color: save errors.
pub fn danger(theme: &Theme) -> text::Style {
    text::Style { color: Some(theme.extended_palette().danger.base.color) }
}

/// The thin border color of cards and dividers.
fn border_color(theme: &Theme) -> Color {
    theme.extended_palette().background.weak.color
}

/// A 1px line between rows.
pub fn divider(theme: &Theme) -> rule::Style {
    rule::Style { color: border_color(theme), radius: 0.0.into(), fill_mode: rule::FillMode::Full, snap: true }
}

/// A block with a thin border on the content background.
pub fn card(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.base.color)),
        text_color: Some(palette.background.base.text),
        border: Border { color: border_color(theme), width: 1.0, radius: RADIUS.into() },
        ..container::Style::default()
    }
}

/// A small dim pill: digest state, harness.
pub fn badge(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(theme.extended_palette().background.weak.color)),
        text_color: Some(muted_color(theme)),
        border: border::rounded(PILL_RADIUS),
        ..container::Style::default()
    }
}

/// A pill for a digest that is current: a faint success tint.
pub fn badge_good(theme: &Theme) -> container::Style {
    let success = theme.extended_palette().success.base.color;
    container::Style {
        background: Some(Background::Color(success.scale_alpha(0.12))),
        text_color: Some(success),
        border: border::rounded(PILL_RADIUS),
        ..container::Style::default()
    }
}

/// A pill for a digest that is out of date: a faint warning tint.
pub fn badge_warn(theme: &Theme) -> container::Style {
    let warning = theme.extended_palette().warning.base.color;
    container::Style {
        background: Some(Background::Color(warning.scale_alpha(0.12))),
        text_color: Some(warning),
        border: border::rounded(PILL_RADIUS),
        ..container::Style::default()
    }
}

/// A small flat button: no fill until hovered.
pub fn flat(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let background = match status {
        button::Status::Hovered => Some(Background::Color(palette.background.weak.color)),
        button::Status::Pressed => Some(Background::Color(palette.background.strong.color)),
        button::Status::Active | button::Status::Disabled => None,
    };
    let text_color = match status {
        button::Status::Disabled => palette.background.base.text.scale_alpha(0.4),
        _ => palette.background.base.text,
    };
    button::Style { background, text_color, border: border::rounded(RADIUS), ..button::Style::default() }
}

/// A raised text field.
pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let palette = theme.extended_palette();
    let border = match status {
        text_input::Status::Focused { .. } => palette.background.strongest.color,
        text_input::Status::Hovered => palette.background.strong.color,
        text_input::Status::Active | text_input::Status::Disabled => palette.background.neutral.color,
    };
    text_input::Style {
        background: Background::Color(palette.background.weakest.color),
        border: Border { color: border, width: 1.0, radius: RADIUS.into() },
        icon: muted_color(theme),
        placeholder: muted_color(theme),
        value: palette.background.base.text,
        selection: palette.background.strongest.color,
    }
}

/// The instructions editor, drawn like [`input`].
pub fn editor(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let palette = theme.extended_palette();
    let border = match status {
        text_editor::Status::Focused { .. } => palette.background.strongest.color,
        text_editor::Status::Hovered => palette.background.strong.color,
        text_editor::Status::Active | text_editor::Status::Disabled => palette.background.neutral.color,
    };
    text_editor::Style {
        background: Background::Color(palette.background.weakest.color),
        border: Border { color: border, width: 1.0, radius: RADIUS.into() },
        placeholder: muted_color(theme),
        value: palette.background.base.text,
        selection: palette.background.strongest.color,
    }
}

/// A 4px scrollbar kept clear of the content.
pub fn thin_scrollbar() -> scrollable::Direction {
    scrollable::Direction::Vertical(scrollable::Scrollbar::new().width(4).scroller_width(4).spacing(2))
}

/// A scroller without a rail, in the border tone.
pub fn scrollbar(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let palette = theme.extended_palette();
    let scroller = match status {
        scrollable::Status::Dragged { .. } | scrollable::Status::Hovered { .. } => palette.background.strongest.color,
        scrollable::Status::Active { .. } => palette.background.strong.color,
    };
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller { background: Background::Color(scroller), border: border::rounded(2) },
    };
    scrollable::Style { vertical_rail: rail, horizontal_rail: rail, ..scrollable::default(theme, status) }
}
