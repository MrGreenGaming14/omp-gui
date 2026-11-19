//! Dark and light themes, the theme preference, and the shared looks every UI
//! crate uses (card, muted text, badge, rows).
//!
//! The palettes follow Claude Code's warm neutrals with our own indigo accent.
//! Everything else reads colors from the extended palette, so a theme switch
//! applies everywhere.

use iced::theme::{Mode, Palette};
use iced::{Subscription, Task, Theme, color};
use std::fmt;

/// Palette of the dark theme.
pub const DARK_PALETTE: Palette = Palette {
    background: color!(0x262624),
    text: color!(0xece9e2),
    primary: color!(0x7c8cff),
    success: color!(0x6bb38a),
    warning: color!(0xe3a84f),
    danger: color!(0xe5675e),
};

/// Palette of the light theme.
pub const LIGHT_PALETTE: Palette = Palette {
    background: color!(0xfaf9f5),
    text: color!(0x1f1e1d),
    primary: color!(0x4a5bd4),
    success: color!(0x2f8f5b),
    warning: color!(0xb7791f),
    danger: color!(0xc8463d),
};

/// Builds the dark theme.
pub fn dark() -> Theme {
    Theme::custom("Loa Dark", DARK_PALETTE)
}

/// Builds the light theme.
pub fn light() -> Theme {
    Theme::custom("Loa Light", LIGHT_PALETTE)
}

/// Which theme the user wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preference {
    /// Follow the operating system.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl Preference {
    /// Every preference, in picker order.
    pub const ALL: [Preference; 3] = [Preference::System, Preference::Light, Preference::Dark];
}

impl fmt::Display for Preference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Preference::System => "System",
            Preference::Light => "Light",
            Preference::Dark => "Dark",
        })
    }
}

/// The two themes the app ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// [`light`].
    Light,
    /// [`dark`].
    Dark,
}

/// The active theme: the user's preference plus the system mode it follows.
#[derive(Debug, Clone)]
pub struct State {
    preference: Preference,
    system: Mode,
    light: Theme,
    dark: Theme,
}

impl State {
    /// Starts with the given preference and an unknown system mode.
    pub fn new(preference: Preference) -> Self {
        Self { preference, system: Mode::None, light: light(), dark: dark() }
    }

    /// Asks the system for its current mode. Feed the result to
    /// [`State::set_system_mode`].
    pub fn detect() -> Task<Mode> {
        iced::system::theme()
    }

    /// System mode changes. Feed each to [`State::set_system_mode`].
    pub fn subscription() -> Subscription<Mode> {
        iced::system::theme_changes()
    }

    /// The user's preference.
    pub fn preference(&self) -> Preference {
        self.preference
    }

    /// Replaces the user's preference.
    pub fn set_preference(&mut self, preference: Preference) {
        self.preference = preference;
    }

    /// Records the system mode. Used while the preference is
    /// [`Preference::System`].
    pub fn set_system_mode(&mut self, mode: Mode) {
        self.system = mode;
    }

    /// The theme in effect. An unknown system mode falls back to dark.
    pub fn variant(&self) -> Variant {
        match self.preference {
            Preference::Light => Variant::Light,
            Preference::Dark => Variant::Dark,
            Preference::System => match self.system {
                Mode::Light => Variant::Light,
                Mode::Dark | Mode::None => Variant::Dark,
            },
        }
    }

    /// The `iced` theme to return from the application's `theme` function.
    pub fn theme(&self) -> Theme {
        match self.variant() {
            Variant::Light => self.light.clone(),
            Variant::Dark => self.dark.clone(),
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new(Preference::default())
    }
}

/// Style functions for the looks shared across the app.
pub mod style {
    use iced::widget::{button, container, text};
    use iced::{Background, Border, Color, Theme, border};

    /// Corner radius of cards and rows.
    pub const RADIUS: f32 = 8.0;

    /// Main content background (chat, panels).
    pub fn surface(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(Background::Color(palette.background.base.color)),
            text_color: Some(palette.background.base.text),
            ..container::Style::default()
        }
    }

    /// Side areas set apart from the content: sidebar, top bar, tab bar.
    pub fn chrome(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(Background::Color(palette.background.weak.color)),
            text_color: Some(palette.background.weak.text),
            ..container::Style::default()
        }
    }

    /// A raised block with a thin border: decision cards, tool rows.
    pub fn card(theme: &Theme) -> container::Style {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(Background::Color(palette.background.weakest.color)),
            text_color: Some(palette.background.weakest.text),
            border: Border { color: palette.background.strong.color, width: 1.0, radius: RADIUS.into() },
            ..container::Style::default()
        }
    }

    /// Secondary text: section headers, node tags, hints.
    pub fn muted(theme: &Theme) -> text::Style {
        text::Style { color: Some(muted_color(theme)) }
    }

    /// The color of [`muted`] text.
    pub fn muted_color(theme: &Theme) -> Color {
        theme.extended_palette().background.base.text.scale_alpha(0.6)
    }

    /// A small neutral pill: node tags, counts.
    pub fn badge(theme: &Theme) -> container::Style {
        let pair = theme.extended_palette().background.strong;
        container::Style {
            background: Some(Background::Color(pair.color)),
            text_color: Some(pair.text),
            border: border::rounded(RADIUS),
            ..container::Style::default()
        }
    }

    /// A pill that asks for attention: a session waiting for a decision.
    pub fn waiting_badge(theme: &Theme) -> container::Style {
        let pair = theme.extended_palette().warning.base;
        container::Style {
            background: Some(Background::Color(pair.color)),
            text_color: Some(pair.text),
            border: border::rounded(RADIUS),
            ..container::Style::default()
        }
    }

    /// A dot telling whether a node is online.
    pub fn status_dot(online: bool) -> impl Fn(&Theme) -> container::Style {
        move |theme| {
            let palette = theme.extended_palette();
            let color = if online { palette.success.base.color } else { palette.danger.base.color };
            container::Style {
                background: Some(Background::Color(color)),
                border: border::rounded(RADIUS),
                ..container::Style::default()
            }
        }
    }

    /// A list row or nav item that is not selected.
    pub fn row(theme: &Theme, status: button::Status) -> button::Style {
        let palette = theme.extended_palette();
        let background = match status {
            button::Status::Hovered => Some(Background::Color(palette.background.strong.color.scale_alpha(0.5))),
            button::Status::Pressed => Some(Background::Color(palette.background.strong.color)),
            button::Status::Active | button::Status::Disabled => None,
        };
        button::Style {
            background,
            text_color: palette.background.weak.text,
            border: border::rounded(RADIUS),
            ..button::Style::default()
        }
    }

    /// The selected list row, or the active tab.
    pub fn selected_row(theme: &Theme, status: button::Status) -> button::Style {
        let palette = theme.extended_palette();
        let color = match status {
            button::Status::Pressed => palette.background.strongest.color,
            _ => palette.background.strong.color,
        };
        button::Style {
            background: Some(Background::Color(color)),
            text_color: palette.background.strong.text,
            border: border::rounded(RADIUS),
            ..button::Style::default()
        }
    }

    /// The row style for a selection state.
    pub fn row_for(selected: bool) -> fn(&Theme, button::Status) -> button::Style {
        if selected { selected_row } else { row }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_preference_follows_system_mode_and_override_wins() {
        let mut state = State::new(Preference::System);
        assert_eq!(state.variant(), Variant::Dark, "unknown system mode falls back to dark");

        state.set_system_mode(Mode::Light);
        assert!(!state.theme().extended_palette().is_dark);

        state.set_preference(Preference::Dark);
        assert!(state.theme().extended_palette().is_dark);

        state.set_system_mode(Mode::Dark);
        state.set_preference(Preference::Light);
        assert_eq!(state.theme().palette(), LIGHT_PALETTE);
    }

    #[test]
    fn badge_text_is_readable_on_both_themes() {
        for theme in [dark(), light()] {
            let badge = style::waiting_badge(&theme);
            let (Some(iced::Background::Color(bg)), Some(fg)) = (badge.background, badge.text_color) else {
                panic!("badge must set background and text colors");
            };
            assert!(contrast(bg, fg) >= 4.5, "{theme}: contrast {}", contrast(bg, fg));
        }
    }

    fn luminance(color: iced::Color) -> f32 {
        let channel = |c: f32| if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
    }

    fn contrast(a: iced::Color, b: iced::Color) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }
}
