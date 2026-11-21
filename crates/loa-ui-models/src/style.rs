//! Looks of the model screens, derived from the theme's extended palette.
//!
//! The app's shared looks live in `loa-ui-shell`, which this crate cannot
//! depend on, so the ones the screens need are mirrored here.

use iced::widget::{button, container, pick_list, row, rule, scrollable, text, text_input};
use iced::{Background, Border, Color, Element, Theme, border};
use loa_ui_kit::icons::Icon;

/// Corner radius of cards, fields and buttons.
const RADIUS: f32 = 6.0;

/// Primary text: names and field values.
pub const BODY: f32 = 14.0;
/// Secondary text: details, hints, buttons.
pub const SMALL: f32 = 12.0;
/// Notes, status lines and empty states.
pub const NOTE: f32 = 13.0;

/// Secondary text color: about 60% of the text color.
pub fn muted_color(theme: &Theme) -> Color {
    theme.extended_palette().background.base.text.scale_alpha(0.6)
}

/// Secondary text: descriptions, hints, inactive choices.
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

/// A raised block with a thin border: search boxes and open editors.
pub fn card(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.weakest.color)),
        text_color: Some(palette.background.weakest.text),
        border: Border { color: palette.background.weak.color, width: 1.0, radius: RADIUS.into() },
        ..container::Style::default()
    }
}

/// A floating popup on the page background with a thin border.
pub fn popup(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.base.color)),
        text_color: Some(palette.background.base.text),
        border: Border { color: palette.background.weak.color, width: 1.0, radius: 8.0.into() },
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

/// A status dot: green when `on`, muted otherwise.
pub fn dot(on: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| container::Style {
        background: Some(Background::Color(if on {
            theme.extended_palette().success.base.color
        } else {
            muted_color(theme)
        })),
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

/// A flat button for destructive actions.
pub fn danger_flat(theme: &Theme, status: button::Status) -> button::Style {
    button::Style { text_color: theme.extended_palette().danger.base.color, ..flat(theme, status) }
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
        pick_list::Status::Hovered | pick_list::Status::Opened { .. } => palette.background.strong.color,
    };
    pick_list::Style {
        text_color: palette.background.base.text,
        placeholder_color: muted_color(theme),
        handle_color: muted_color(theme),
        background: Background::Color(palette.background.weakest.color),
        border: Border { color: border_color, width: 1.0, radius: RADIUS.into() },
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
        border: Border { color: border_color, width: 1.0, radius: RADIUS.into() },
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

/// A small flat text button.
pub fn flat_button<'a, M: 'a>(label: impl text::IntoFragment<'a>) -> button::Button<'a, M> {
    button(text(label).size(SMALL)).padding([4.0, 8.0]).style(flat)
}

/// A small flat button in the danger color.
pub fn danger_button<'a, M: 'a>(label: impl text::IntoFragment<'a>) -> button::Button<'a, M> {
    flat_button(label).style(danger_flat)
}

/// The one filled action of a block: small and neutral.
pub fn filled_button<'a, M: 'a>(label: impl text::IntoFragment<'a>) -> button::Button<'a, M> {
    button(text(label).size(SMALL)).padding([4.0, 10.0]).style(button::primary)
}

/// A small flat button holding only a muted icon.
pub fn icon_button<'a, M: 'a>(icon: Icon) -> button::Button<'a, M> {
    button(loa_ui_kit::icons::muted(icon, 12.0)).padding(4).style(flat)
}

/// A flat button with a leading icon.
pub fn icon_label_button<'a, M: 'a>(icon: Icon, label: impl text::IntoFragment<'a>) -> button::Button<'a, M> {
    button(row![loa_ui_kit::icons::icon(icon, 12.0), text(label).size(SMALL)].spacing(6).align_y(iced::Center))
        .padding([4.0, 8.0])
        .style(flat)
}

/// A raised text field.
pub fn field<'a, M: Clone + 'a>(placeholder: &str, value: &str) -> text_input::TextInput<'a, M> {
    text_input(placeholder, value).size(BODY).padding([6.0, 8.0]).style(input)
}

/// A raised search box with a magnifier icon around a borderless field.
pub fn search_box<'a, M: Clone + 'a>(input: text_input::TextInput<'a, M>) -> Element<'a, M> {
    container(
        row![loa_ui_kit::icons::muted(Icon::Search, 14.0), input.size(BODY).padding(0.0).style(bare)]
            .spacing(8)
            .align_y(iced::Center),
    )
    .padding([6.0, 10.0])
    .width(iced::Fill)
    .style(card)
    .into()
}
