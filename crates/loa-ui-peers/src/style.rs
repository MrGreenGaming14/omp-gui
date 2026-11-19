//! Looks of the peers screens, derived from the theme's extended palette.
//!
//! The app's shared looks live in `loa-ui-shell`, which this crate cannot
//! depend on, so the ones the screens need are mirrored here.

use iced::widget::{button, container, pick_list, rule, scrollable, text, text_input};
use iced::{Background, Border, Color, Theme, border};

/// Corner radius of cards, fields and buttons.
const RADIUS: f32 = 6.0;

/// Secondary text color: about 60% of the text color.
pub fn muted_color(theme: &Theme) -> Color {
    theme.extended_palette().background.base.text.scale_alpha(0.6)
}

/// Secondary text: descriptions, hints, inactive choices.
pub fn muted(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(muted_color(theme)),
    }
}

/// An uppercase section header in small muted text.
pub fn section_header<'a>(title: impl AsRef<str>) -> text::Text<'a> {
    text(title.as_ref().to_uppercase()).size(12).style(muted)
}

/// A 1px line between rows and sections.
pub fn divider<'a>() -> rule::Rule<'a> {
    rule::horizontal(1).style(|theme: &Theme| rule::Style {
        color: theme.extended_palette().background.weak.color,
        radius: 0.0.into(),
        fill_mode: rule::FillMode::Full,
        snap: true,
    })
}

/// A raised block with a thin border, for the search box.
pub fn card(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.weakest.color)),
        text_color: Some(palette.background.weakest.text),
        border: Border {
            color: palette.background.weak.color,
            width: 1.0,
            radius: RADIUS.into(),
        },
        ..container::Style::default()
    }
}

/// What a status dot says about its peer.
#[derive(Clone, Copy)]
pub enum Tone {
    /// Online.
    Ok,
    /// Offline.
    Off,
}

fn tone_color(theme: &Theme, tone: Tone) -> Color {
    let palette = theme.extended_palette();
    match tone {
        Tone::Ok => palette.success.base.color,
        Tone::Off => muted_color(theme),
    }
}

/// A 6px status dot.
pub fn dot(tone: Tone) -> impl Fn(&Theme) -> container::Style {
    move |theme| container::Style {
        background: Some(Background::Color(tone_color(theme, tone))),
        border: border::rounded(3.0),
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
    button::Style {
        background,
        text_color,
        border: border::rounded(RADIUS),
        ..button::Style::default()
    }
}

/// A flat button for destructive actions.
pub fn danger_flat(theme: &Theme, status: button::Status) -> button::Style {
    button::Style {
        text_color: theme.extended_palette().danger.base.color,
        ..flat(theme, status)
    }
}

/// The current choice in a list.
pub fn selected(theme: &Theme, _status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    button::Style {
        background: Some(Background::Color(palette.background.weak.color)),
        text_color: palette.background.base.text,
        border: border::rounded(RADIUS),
        ..button::Style::default()
    }
}

/// A pick list on a raised field.
pub fn pick(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let palette = theme.extended_palette();
    let border_color = match status {
        pick_list::Status::Active => palette.background.weak.color,
        pick_list::Status::Hovered | pick_list::Status::Opened { .. } => {
            palette.background.strong.color
        }
    };
    pick_list::Style {
        text_color: palette.background.base.text,
        placeholder_color: muted_color(theme),
        handle_color: muted_color(theme),
        background: Background::Color(palette.background.weakest.color),
        border: Border {
            color: border_color,
            width: 1.0,
            radius: RADIUS.into(),
        },
    }
}

/// A text field on a raised block.
pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let palette = theme.extended_palette();
    let border_color = match status {
        text_input::Status::Focused { .. } => palette.background.strong.color,
        _ => palette.background.weak.color,
    };
    text_input::Style {
        background: Background::Color(palette.background.weakest.color),
        border: Border {
            color: border_color,
            width: 1.0,
            radius: RADIUS.into(),
        },
        icon: muted_color(theme),
        placeholder: muted_color(theme),
        value: palette.background.base.text,
        selection: palette.primary.weak.color,
    }
}

/// A text field without fill or border, for use inside a raised block.
pub fn bare(theme: &Theme, _status: text_input::Status) -> text_input::Style {
    let palette = theme.extended_palette();
    text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        icon: muted_color(theme),
        placeholder: muted_color(theme),
        value: palette.background.base.text,
        selection: palette.primary.weak.color,
    }
}

/// A 4px scrollbar kept clear of the content.
pub fn thin_scrollbar() -> scrollable::Direction {
    scrollable::Direction::Vertical(
        scrollable::Scrollbar::new()
            .width(4)
            .scroller_width(4)
            .spacing(2),
    )
}

/// A scroller without a rail, in the border tone.
pub fn scrollbar(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let palette = theme.extended_palette();
    let scroller = match status {
        scrollable::Status::Dragged { .. } | scrollable::Status::Hovered { .. } => {
            palette.background.strongest.color
        }
        scrollable::Status::Active { .. } => palette.background.strong.color,
    };
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(scroller),
            border: border::rounded(2),
        },
    };
    scrollable::Style {
        vertical_rail: rail,
        horizontal_rail: rail,
        ..scrollable::default(theme, status)
    }
}
