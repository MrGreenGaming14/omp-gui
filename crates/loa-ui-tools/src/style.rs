//! Looks of the tools panel.
//!
//! The app's shared looks live in `loa-ui-shell`, which this crate cannot
//! depend on, so the tokens and looks the panel needs are mirrored here.

use iced::widget::{
    button, checkbox, container, pick_list, rule, scrollable, text, text_input,
};
use iced::{Background, Border, Color, Theme, border, color};

/// Corner radius of fields, rows and small buttons.
const RADIUS: f32 = 6.0;
/// Corner radius of cards.
const CARD_RADIUS: f32 = 12.0;

/// The shell's theme colors the panel uses.
struct Tokens {
    card: Color,
    hover: Color,
    selected: Color,
    line: Color,
    line_strong: Color,
    text: Color,
    text2: Color,
    text3: Color,
    accent: Color,
    on_accent: Color,
    warn: Color,
}

const DARK: Tokens = Tokens {
    card: color!(0x2b2a28),
    hover: color!(0x34332f),
    selected: color!(0x34332f),
    line: color!(0x33322f),
    line_strong: color!(0x4a4945),
    text: color!(0xecebe6),
    text2: color!(0xb7b5ac),
    text3: color!(0x9a988f),
    accent: color!(0x7fb7a4),
    on_accent: color!(0x1f1e1d),
    warn: color!(0xf0b866),
};

const LIGHT: Tokens = Tokens {
    card: color!(0xffffff),
    hover: color!(0xebe8df),
    selected: color!(0xe4e0d5),
    line: color!(0xe3e0d6),
    line_strong: color!(0xcbc7bb),
    text: color!(0x1f1e1d),
    text2: color!(0x55534d),
    text3: color!(0x6f6d66),
    accent: color!(0x2f7a66),
    on_accent: color!(0xffffff),
    warn: color!(0x8a560c),
};

fn tokens(theme: &Theme) -> &'static Tokens {
    if theme.extended_palette().is_dark {
        &DARK
    } else {
        &LIGHT
    }
}

/// Secondary text color.
pub fn muted_color(theme: &Theme) -> Color {
    tokens(theme).text2
}

/// Secondary text: descriptions, hints, inactive tabs.
pub fn muted(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(muted_color(theme)),
    }
}

/// Tertiary text: counts, timestamps, metadata.
pub fn faint(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(tokens(theme).text3),
    }
}

/// Text in the needs-you color.
pub fn warn_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(tokens(theme).warn),
    }
}

/// A 1px line between rows and sections.
pub fn divider<'a>() -> rule::Rule<'a> {
    rule::horizontal(1).style(|theme: &Theme| rule::Style {
        color: tokens(theme).line,
        radius: 0.0.into(),
        fill_mode: rule::FillMode::Full,
        snap: true,
    })
}

/// A bordered card: groups of rows, forms and open editors.
pub fn card(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(Background::Color(t.card)),
        text_color: Some(t.text),
        border: Border {
            color: t.line,
            width: 1.0,
            radius: CARD_RADIUS.into(),
        },
        ..container::Style::default()
    }
}

/// A card that waits for the user: host key and approval questions.
pub fn attention_card(theme: &Theme) -> container::Style {
    container::Style {
        border: Border {
            color: tokens(theme).warn.scale_alpha(0.35),
            width: 1.0,
            radius: CARD_RADIUS.into(),
        },
        ..card(theme)
    }
}

/// An inset block inside a card: the open details of a row.
pub fn inset(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(Background::Color(t.hover.scale_alpha(0.5))),
        ..container::Style::default()
    }
}

/// A clickable row inside a card: no fill until hovered.
pub fn row(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let background = match status {
        button::Status::Hovered => Some(Background::Color(t.hover)),
        button::Status::Pressed => Some(Background::Color(t.selected)),
        button::Status::Active | button::Status::Disabled => None,
    };
    button::Style {
        background,
        text_color: t.text,
        border: border::rounded(RADIUS),
        ..button::Style::default()
    }
}

/// A clickable header above a card: text brightens when hovered.
pub fn header(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let text_color = match status {
        button::Status::Hovered | button::Status::Pressed => t.text,
        _ => t.text2,
    };
    button::Style {
        text_color,
        ..button::Style::default()
    }
}

/// A checkbox: outlined when off, accent filled when on.
pub fn check(theme: &Theme, status: checkbox::Status) -> checkbox::Style {
    let t = tokens(theme);
    let (checked, hovered, disabled) = match status {
        checkbox::Status::Active { is_checked } => (is_checked, false, false),
        checkbox::Status::Hovered { is_checked } => (is_checked, true, false),
        checkbox::Status::Disabled { is_checked } => (is_checked, false, true),
    };
    let alpha = if disabled { 0.45 } else { 1.0 };
    let (background, border_color) = if checked {
        (t.accent.scale_alpha(alpha), t.accent.scale_alpha(alpha))
    } else if hovered {
        (Color::TRANSPARENT, t.text3)
    } else {
        (Color::TRANSPARENT, t.line_strong.scale_alpha(alpha))
    };
    checkbox::Style {
        background: Background::Color(background),
        icon_color: t.on_accent,
        border: Border {
            color: border_color,
            width: 1.0,
            radius: 4.0.into(),
        },
        text_color: Some(t.text.scale_alpha(alpha)),
    }
}

/// What a status dot or label says about its item.
#[derive(Clone, Copy)]
pub enum Tone {
    /// Running, connected, allowed.
    Ok,
    /// Stopped, offline, not allowed.
    Off,
    /// Failed.
    Bad,
}

fn tone_color(theme: &Theme, tone: Tone) -> Color {
    let palette = theme.extended_palette();
    match tone {
        Tone::Ok => palette.success.base.color,
        Tone::Off => muted_color(theme),
        Tone::Bad => palette.danger.base.color,
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

/// Status text in the color of its tone.
pub fn tone_text(tone: Tone) -> impl Fn(&Theme) -> text::Style {
    move |theme| text::Style { color: Some(tone_color(theme, tone)) }
}

/// A small flat button: no fill until hovered.
pub fn flat(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let background = match status {
        button::Status::Hovered => Some(Background::Color(t.hover)),
        button::Status::Pressed => Some(Background::Color(t.selected)),
        button::Status::Active | button::Status::Disabled => None,
    };
    let text_color = match status {
        button::Status::Disabled => t.text3.scale_alpha(0.6),
        button::Status::Hovered | button::Status::Pressed => t.text,
        button::Status::Active => t.text2,
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

/// The current choice of a segmented control.
pub fn selected(theme: &Theme, _status: button::Status) -> button::Style {
    let t = tokens(theme);
    button::Style {
        background: Some(Background::Color(t.selected)),
        text_color: t.text,
        border: border::rounded(RADIUS),
        ..button::Style::default()
    }
}

/// A button with no fill or border; its content carries the look.
pub fn plain(_theme: &Theme, _status: button::Status) -> button::Style {
    button::Style::default()
}

/// The underline of a tab: bright when active, invisible otherwise.
pub fn underline(active: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| container::Style {
        background: active.then(|| Background::Color(tokens(theme).accent)),
        ..container::Style::default()
    }
}

/// A pick list on a bordered field.
pub fn pick(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let t = tokens(theme);
    let border_color = match status {
        pick_list::Status::Active => t.line_strong,
        pick_list::Status::Hovered | pick_list::Status::Opened { .. } => t.text3,
    };
    pick_list::Style {
        text_color: t.text,
        placeholder_color: t.text3,
        handle_color: t.text3,
        background: Background::Color(t.card),
        border: Border {
            color: border_color,
            width: 1.0,
            radius: RADIUS.into(),
        },
    }
}

/// A text field with a thin border.
pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let t = tokens(theme);
    let border_color = match status {
        text_input::Status::Focused { .. } => t.accent,
        text_input::Status::Hovered => t.line_strong,
        _ => t.line,
    };
    text_input::Style {
        background: Background::Color(t.card),
        border: Border {
            color: border_color,
            width: 1.0,
            radius: RADIUS.into(),
        },
        icon: t.text3,
        placeholder: t.text3,
        value: t.text,
        selection: t.accent.scale_alpha(0.35),
    }
}

/// A search box: a bordered field whose input is [`bare`].
pub fn search_box(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(Background::Color(t.card)),
        border: Border {
            color: t.line_strong,
            width: 1.0,
            radius: RADIUS.into(),
        },
        ..container::Style::default()
    }
}

/// A text field without fill or border, for use inside a raised block.
pub fn bare(theme: &Theme, _status: text_input::Status) -> text_input::Style {
    let t = tokens(theme);
    text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        icon: t.text3,
        placeholder: t.text3,
        value: t.text,
        selection: t.accent.scale_alpha(0.35),
    }
}

/// A 4px scrollbar kept clear of the content.
pub fn thin_scrollbar() -> scrollable::Direction {
    scrollable::Direction::Vertical(
        scrollable::Scrollbar::new()
            .width(4)
            .scroller_width(4)
            .spacing(6),
    )
}

/// A scroller without a rail, in the border tone.
pub fn scrollbar(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let t = tokens(theme);
    let scroller = match status {
        scrollable::Status::Dragged { .. } | scrollable::Status::Hovered { .. } => t.text3,
        scrollable::Status::Active { .. } => t.line_strong,
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
