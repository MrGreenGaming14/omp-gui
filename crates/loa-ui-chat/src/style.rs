//! Looks of the transcript. Every color comes from the theme's extended
//! palette, so the dark and light themes both apply.

use iced::widget::{button, container, markdown, rule, text};
use iced::{Background, Border, Color, Font, Padding, Theme, border};

/// Body text size.
pub(crate) const TEXT_SIZE: f32 = 14.0;
/// Secondary text: tool details, notes under a message.
pub(crate) const SMALL_SIZE: f32 = 13.0;
/// Line height of body text, relative to its size.
pub(crate) const LINE_HEIGHT: f32 = 1.6;
/// Size of row icons and chevrons.
pub(crate) const ICON_SIZE: f32 = 14.0;
pub(crate) const RADIUS: f32 = 8.0;

/// Opacity of muted text and icons.
const MUTED_ALPHA: f32 = 0.55;

/// Theme-dependent settings resolved once per theme, for widgets that take
/// colors rather than style functions.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Look {
    pub markdown: markdown::Settings,
    /// The color of muted text.
    pub muted: Color,
}

impl Look {
    pub(crate) fn new(theme: &Theme) -> Self {
        let palette = theme.extended_palette();
        let style = markdown::Style {
            font: Font::DEFAULT,
            inline_code_highlight: markdown::Highlight {
                background: Background::Color(palette.background.weak.color),
                border: border::rounded(4),
            },
            inline_code_padding: Padding::ZERO.left(2).right(2),
            inline_code_color: palette.background.base.text,
            inline_code_font: Font::MONOSPACE,
            code_block_font: Font::MONOSPACE,
            link_color: palette.primary.base.color,
        };
        let mut markdown = markdown::Settings::with_text_size(TEXT_SIZE, style);
        markdown.code_size = SMALL_SIZE.into();
        markdown.spacing = 12.into();
        Self {
            markdown,
            muted: muted_color(theme),
        }
    }
}

pub(crate) fn muted_color(theme: &Theme) -> Color {
    theme.extended_palette().background.base.text.scale_alpha(MUTED_ALPHA)
}

/// Secondary text.
pub(crate) fn muted(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(muted_color(theme)),
    }
}

/// A row header that toggles open and closed: no chrome until hovered.
pub(crate) fn header(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => {
            Some(Background::Color(palette.background.weaker.color))
        }
        button::Status::Active | button::Status::Disabled => None,
    };
    button::Style {
        background,
        text_color: palette.background.base.text,
        border: border::rounded(6),
        ..button::Style::default()
    }
}

/// A user message: a raised bubble.
pub(crate) fn bubble(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let pair = match status {
        button::Status::Hovered | button::Status::Pressed => palette.background.strong,
        button::Status::Active | button::Status::Disabled => palette.background.weak,
    };
    button::Style {
        background: Some(Background::Color(pair.color)),
        text_color: pair.text,
        border: border::rounded(RADIUS),
        ..button::Style::default()
    }
}

/// Monospace output and code blocks: a raised field with a thin border.
pub(crate) fn block(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style {
        background: Some(Background::Color(palette.background.weakest.color)),
        text_color: Some(palette.background.weakest.text),
        border: Border {
            color: palette.background.weak.color,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    }
}

/// The thin line beside the details of an expanded row.
pub(crate) fn guide(theme: &Theme) -> rule::Style {
    rule::Style {
        color: theme.extended_palette().background.weak.color,
        radius: 0.0.into(),
        fill_mode: rule::FillMode::Full,
        snap: true,
    }
}

/// The "Jump to latest" pill floating over the transcript.
pub(crate) fn jump(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let pair = match status {
        button::Status::Hovered | button::Status::Pressed => palette.background.strong,
        button::Status::Active | button::Status::Disabled => palette.background.weak,
    };
    button::Style {
        background: Some(Background::Color(pair.color)),
        text_color: pair.text,
        border: Border {
            color: palette.background.strongest.color,
            width: 1.0,
            radius: 999.0.into(),
        },
        ..button::Style::default()
    }
}
