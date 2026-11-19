//! Looks of the Updates screen, derived from the theme's extended palette.
//!
//! The app's shared looks live in `loa-ui-shell`, which this crate cannot
//! depend on, so the ones the screen needs are mirrored here.

use iced::widget::{button, container, rule, scrollable, text};
use iced::{Background, Border, Color, Theme, border};

/// Corner radius of cards and buttons.
const RADIUS: f32 = 6.0;

/// Primary text: product names and versions.
pub const BODY: f32 = 14.0;
/// Secondary text: details, buttons.
pub const SMALL: f32 = 12.0;
/// Status lines and empty states.
pub const NOTE: f32 = 13.0;

/// What a status line or check says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Good,
    Attention,
    Bad,
}

pub fn tone_color(theme: &Theme, tone: Tone) -> Color {
    let palette = theme.extended_palette();
    match tone {
        Tone::Neutral => muted_color(theme),
        Tone::Good => palette.success.base.color,
        Tone::Attention => palette.primary.base.color,
        Tone::Bad => palette.danger.base.color,
    }
}

/// Text in `tone`'s color.
pub fn toned(tone: Tone) -> impl Fn(&Theme) -> text::Style {
    move |theme| text::Style { color: Some(tone_color(theme, tone)) }
}

/// Secondary text color: about 60% of the text color.
pub fn muted_color(theme: &Theme) -> Color {
    theme.extended_palette().background.base.text.scale_alpha(0.6)
}

/// Secondary text: descriptions, hints.
pub fn muted(theme: &Theme) -> text::Style {
    text::Style { color: Some(muted_color(theme)) }
}

/// An uppercase section header in small muted text.
pub fn section_header<'a>(title: impl AsRef<str>) -> text::Text<'a> {
    text(title.as_ref().to_uppercase()).size(SMALL).style(muted)
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

/// A raised block with a thin border: one product.
pub fn card(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.weakest.color)),
        text_color: Some(palette.background.weakest.text),
        border: Border { color: palette.background.weak.color, width: 1.0, radius: RADIUS.into() },
        ..container::Style::default()
    }
}

/// A box of errors: a faint danger tint under a danger border.
pub fn error_box(theme: &Theme) -> container::Style {
    let danger = theme.extended_palette().danger.base.color;
    container::Style {
        background: Some(Background::Color(danger.scale_alpha(0.08))),
        text_color: Some(danger),
        border: Border { color: danger.scale_alpha(0.4), width: 1.0, radius: RADIUS.into() },
        ..container::Style::default()
    }
}

/// A small pill naming the kind of product.
pub fn badge(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.weak.color)),
        text_color: Some(muted_color(theme)),
        border: border::rounded(RADIUS),
        ..container::Style::default()
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
    button::Style { background, text_color, border: border::rounded(RADIUS), ..button::Style::default() }
}

/// A flat button for risky actions.
pub fn danger_flat(theme: &Theme, status: button::Status) -> button::Style {
    button::Style { text_color: theme.extended_palette().danger.base.color, ..flat(theme, status) }
}

/// A small flat text button.
pub fn flat_button<'a, M: 'a>(label: impl text::IntoFragment<'a>) -> button::Button<'a, M> {
    button(text(label).size(SMALL)).padding([4.0, 8.0]).style(flat)
}

/// A small flat button in the danger color.
pub fn danger_button<'a, M: 'a>(label: impl text::IntoFragment<'a>) -> button::Button<'a, M> {
    flat_button(label).style(danger_flat)
}

/// The one filled action of a block.
pub fn filled_button<'a, M: 'a>(label: impl text::IntoFragment<'a>) -> button::Button<'a, M> {
    button(text(label).size(SMALL)).padding([4.0, 10.0]).style(button::primary)
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
