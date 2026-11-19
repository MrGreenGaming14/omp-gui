//! Looks of the Surfaces tab. Shared looks come from the shell theme; the
//! rest read the theme's extended palette, so dark and light both apply.

use iced::widget::{button, container, text};
use iced::{Background, Border, Theme};
use loa_ui_shell::theme::style as shell;

/// Text size of rows and fields, matching the other panel tabs.
pub const TEXT: f32 = 13.0;

/// Text size of hints and captions.
pub const SMALL: f32 = 12.0;

/// Text in the warning color: problems the user can act on.
pub fn warning(theme: &Theme) -> text::Style {
    text::Style { color: Some(theme.extended_palette().warning.base.color) }
}

/// Text in the danger color: failures.
pub fn danger(theme: &Theme) -> text::Style {
    text::Style { color: Some(theme.extended_palette().danger.base.color) }
}

/// The action button of a form: the strong neutral of the palette.
pub fn action(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let pair = match status {
        button::Status::Hovered | button::Status::Pressed => palette.primary.strong,
        button::Status::Active => palette.primary.base,
        button::Status::Disabled => palette.background.weak,
    };
    button::Style {
        background: Some(Background::Color(pair.color)),
        text_color: pair.text,
        border: Border { radius: shell::RADIUS.into(), ..Border::default() },
        ..button::Style::default()
    }
}

/// The latency pill: muted while within budget, warning past it.
pub fn latency(warning: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        if warning {
            container::Style { text_color: Some(theme.extended_palette().warning.base.color), ..shell::badge(theme) }
        } else {
            shell::badge(theme)
        }
    }
}
