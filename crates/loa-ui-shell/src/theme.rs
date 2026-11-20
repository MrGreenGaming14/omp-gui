//! Dark and light themes, the theme preference, and the shared looks every UI
//! crate uses (card, muted text, badge, rows).
//!
//! The palettes are warm neutrals with one sage accent, matching the v3
//! design: a near-black ground, a darker sidebar and panel, and raised cards
//! a step lighter. Status colors carry the remaining hue. Shell styles read
//! exact colors from [`style::tokens`]; every other crate reads the extended
//! palette, so a theme switch applies everywhere.

use iced::theme::{Mode, Palette};
use iced::{Subscription, Task, Theme, color};
use std::fmt;

/// Palette of the dark theme.
pub const DARK_PALETTE: Palette = Palette {
    background: color!(0x262624),
    text: color!(0xecebe6),
    primary: color!(0x7fb7a4),
    success: color!(0x6fbf73),
    warning: color!(0xf0b866),
    danger: color!(0xe8a0a0),
};

/// Palette of the light theme.
pub const LIGHT_PALETTE: Palette = Palette {
    background: color!(0xfaf9f5),
    text: color!(0x1f1e1d),
    primary: color!(0x2f7a66),
    success: color!(0x2f8f5b),
    warning: color!(0x8a560c),
    danger: color!(0xb3423a),
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
    use iced::widget::{button, container, rule, scrollable, text, text_input};
    use iced::{Background, Border, Color, Shadow, Theme, Vector, border, color};

    /// Corner radius of fields, rows and small buttons.
    pub const RADIUS: f32 = 6.0;

    /// Corner radius of cards, the composer and windows.
    pub const CARD_RADIUS: f32 = 12.0;

    /// Corner radius of small tags such as node names.
    pub const PILL_RADIUS: f32 = 4.0;

    /// The exact colors of one theme. Shell styles use these; other crates
    /// read the extended palette, which iced derives from the same base.
    #[derive(Debug, Clone, Copy)]
    pub struct Tokens {
        /// Chat and content ground.
        pub ground: Color,
        /// Sidebar and side panel.
        pub side: Color,
        /// Cards and windows.
        pub card: Color,
        /// Composer, user messages and selected-tab fills.
        pub raised: Color,
        /// Row hover.
        pub hover: Color,
        /// Selected row.
        pub selected: Color,
        /// Hairlines between areas.
        pub line: Color,
        /// Borders of fields and outlined buttons.
        pub line_strong: Color,
        /// Body text.
        pub text: Color,
        /// Secondary text: labels, hints.
        pub text2: Color,
        /// Tertiary text: counts, timestamps.
        pub text3: Color,
        /// The one accent: send, selection, toggles.
        pub accent: Color,
        /// Text on the accent.
        pub on_accent: Color,
        /// Needs-you color: waiting sessions, approvals.
        pub warn: Color,
        /// Online, added lines.
        pub ok: Color,
        /// Errors, removed lines.
        pub danger: Color,
        /// Window scrim.
        pub scrim: Color,
    }

    /// Dark theme tokens.
    pub const DARK: Tokens = Tokens {
        ground: color!(0x262624),
        side: color!(0x1f1e1d),
        card: color!(0x2b2a28),
        raised: color!(0x30302e),
        hover: color!(0x2b2a28),
        selected: color!(0x34332f),
        line: color!(0x33322f),
        line_strong: color!(0x4a4945),
        text: color!(0xecebe6),
        text2: color!(0xb7b5ac),
        text3: color!(0x9a988f),
        accent: color!(0x7fb7a4),
        on_accent: color!(0x1f1e1d),
        warn: color!(0xf0b866),
        ok: color!(0x6fbf73),
        danger: color!(0xe8a0a0),
        scrim: Color { r: 0.055, g: 0.051, b: 0.047, a: 0.6 },
    };

    /// Light theme tokens.
    pub const LIGHT: Tokens = Tokens {
        ground: color!(0xfaf9f5),
        side: color!(0xf1efe8),
        card: color!(0xffffff),
        raised: color!(0xffffff),
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
        ok: color!(0x2f8f5b),
        danger: color!(0xb3423a),
        scrim: Color { r: 0.1, g: 0.1, b: 0.09, a: 0.35 },
    };

    /// The tokens of this theme.
    pub fn tokens(theme: &Theme) -> &'static Tokens {
        if theme.extended_palette().is_dark { &DARK } else { &LIGHT }
    }

    fn fill(color: Color) -> Option<Background> {
        Some(Background::Color(color))
    }

    /// Main content background (chat, screens).
    pub fn surface(theme: &Theme) -> container::Style {
        let t = tokens(theme);
        container::Style { background: fill(t.ground), text_color: Some(t.text), ..container::Style::default() }
    }

    /// Under the floating composer: clear at the top, fading into the
    /// ground, so the transcript shows through and slides under it.
    pub fn composer_fade(theme: &Theme) -> container::Style {
        let t = tokens(theme);
        let fade = iced::gradient::Linear::new(iced::Degrees(180.0))
            .add_stop(0.0, t.ground.scale_alpha(0.0))
            .add_stop(0.14, t.ground.scale_alpha(0.97))
            .add_stop(1.0, t.ground);
        container::Style { background: Some(Background::Gradient(fade.into())), ..container::Style::default() }
    }

    /// The sidebar and side panel ground, a step darker than [`surface`].
    pub fn side(theme: &Theme) -> container::Style {
        let t = tokens(theme);
        container::Style { background: fill(t.side), text_color: Some(t.text), ..container::Style::default() }
    }

    /// Bars that sit on the content ground: the top bar and tab bars.
    pub fn chrome(theme: &Theme) -> container::Style {
        surface(theme)
    }

    /// The color of thin borders and dividers.
    pub fn border_color(theme: &Theme) -> Color {
        tokens(theme).line
    }

    /// A 1px line between areas.
    pub fn divider(theme: &Theme) -> rule::Style {
        rule::Style { color: border_color(theme), ..rule::default(theme) }
    }

    /// The thin scroller of an overflowing tab bar, without a rail: in the
    /// border tone, stronger while hovered or dragged.
    pub fn tab_scrollbar(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
        let t = tokens(theme);
        let scroller = match status {
            scrollable::Status::Dragged { .. } | scrollable::Status::Hovered { .. } => t.text3,
            scrollable::Status::Active { .. } => t.line_strong,
        };
        let rail = scrollable::Rail {
            background: None,
            border: Border::default(),
            scroller: scrollable::Scroller { background: Background::Color(scroller), border: border::rounded(1) },
        };
        scrollable::Style { vertical_rail: rail, horizontal_rail: rail, ..scrollable::default(theme, status) }
    }

    /// A card: decision prompts, subagents, settings groups.
    pub fn card(theme: &Theme) -> container::Style {
        let t = tokens(theme);
        container::Style {
            background: fill(t.card),
            text_color: Some(t.text),
            border: Border { color: t.line, width: 1.0, radius: CARD_RADIUS.into() },
            ..container::Style::default()
        }
    }

    /// A card that waits for the user: approvals and questions.
    pub fn attention_card(theme: &Theme) -> container::Style {
        let t = tokens(theme);
        container::Style { border: Border { color: t.warn.scale_alpha(0.35), width: 1.0, radius: CARD_RADIUS.into() }, ..card(theme) }
    }

    /// The composer and user message bubbles.
    pub fn raised(theme: &Theme) -> container::Style {
        let t = tokens(theme);
        container::Style {
            background: fill(t.raised),
            text_color: Some(t.text),
            border: Border { color: t.line, width: 1.0, radius: CARD_RADIUS.into() },
            ..container::Style::default()
        }
    }

    /// A floating window: Usage, Settings, menus and pickers.
    pub fn window(theme: &Theme) -> container::Style {
        let t = tokens(theme);
        container::Style {
            background: fill(t.card),
            text_color: Some(t.text),
            border: Border { color: t.line_strong, width: 1.0, radius: CARD_RADIUS.into() },
            shadow: Shadow { color: Color::BLACK.scale_alpha(0.45), offset: Vector::new(0.0, 16.0), blur_radius: 40.0 },
            ..container::Style::default()
        }
    }

    /// The dimmed layer behind a window.
    pub fn scrim(theme: &Theme) -> container::Style {
        container::Style { background: fill(tokens(theme).scrim), ..container::Style::default() }
    }

    /// A text input on the side ground with a thin border.
    pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
        let t = tokens(theme);
        let border = match status {
            text_input::Status::Focused { .. } => t.accent,
            text_input::Status::Hovered => t.line_strong,
            text_input::Status::Active | text_input::Status::Disabled => t.line,
        };
        text_input::Style {
            background: Background::Color(t.side),
            border: Border { color: border, width: 1.0, radius: RADIUS.into() },
            icon: t.text3,
            placeholder: t.text3,
            value: t.text,
            selection: t.accent.scale_alpha(0.35),
        }
    }

    /// Secondary text: labels, hints.
    pub fn muted(theme: &Theme) -> text::Style {
        text::Style { color: Some(muted_color(theme)) }
    }

    /// Tertiary text: counts and timestamps.
    pub fn faint(theme: &Theme) -> text::Style {
        text::Style { color: Some(tokens(theme).text3) }
    }

    /// The color of [`muted`] text.
    pub fn muted_color(theme: &Theme) -> Color {
        tokens(theme).text2
    }

    /// Text in the needs-you color: "Approve", "Needs approval".
    pub fn waiting_text(theme: &Theme) -> text::Style {
        text::Style { color: Some(tokens(theme).warn) }
    }

    /// Text in the accent color.
    pub fn accent_text(theme: &Theme) -> text::Style {
        text::Style { color: Some(tokens(theme).accent) }
    }

    /// A small quiet tag: node names, counts. No fill, just tertiary text.
    pub fn badge(theme: &Theme) -> container::Style {
        container::Style { text_color: Some(tokens(theme).text3), ..container::Style::default() }
    }

    /// A thin outlined block.
    pub fn outlined(theme: &Theme) -> container::Style {
        let t = tokens(theme);
        container::Style { border: Border { color: t.line_strong, width: 1.0, radius: RADIUS.into() }, ..surface(theme) }
    }

    /// The needs-you label on a waiting session: plain warn-colored text, no
    /// fill.
    pub fn waiting_badge(theme: &Theme) -> container::Style {
        container::Style { text_color: Some(tokens(theme).warn), ..container::Style::default() }
    }

    /// A dot telling whether a node is online: green when online, grey when
    /// not.
    pub fn status_dot(online: bool) -> impl Fn(&Theme) -> container::Style {
        move |theme| dot(if online { tokens(theme).ok } else { tokens(theme).text3 })(theme)
    }

    /// A round dot of this color.
    pub fn dot(color: Color) -> impl Fn(&Theme) -> container::Style {
        move |_| container::Style { background: fill(color), border: border::rounded(99), ..container::Style::default() }
    }

    /// A bar track and its fill, for usage meters.
    pub fn meter_track(theme: &Theme) -> container::Style {
        container::Style { background: fill(tokens(theme).line_strong), border: border::rounded(99), ..container::Style::default() }
    }

    /// The filled part of a meter: accent below half, warn from half, danger
    /// from 80%.
    pub fn meter_fill(fraction: f32) -> impl Fn(&Theme) -> container::Style {
        move |theme| {
            let t = tokens(theme);
            let color = if fraction >= 0.8 { t.danger } else if fraction >= 0.5 { t.warn } else { t.accent };
            container::Style { background: fill(color), border: border::rounded(99), ..container::Style::default() }
        }
    }

    /// A list row or nav item that is not selected.
    pub fn row(theme: &Theme, status: button::Status) -> button::Style {
        let t = tokens(theme);
        let background = match status {
            button::Status::Hovered => fill(t.hover),
            button::Status::Pressed => fill(t.selected),
            button::Status::Active | button::Status::Disabled => None,
        };
        button::Style { background, text_color: t.text, border: border::rounded(RADIUS + 2.0), ..button::Style::default() }
    }

    /// The selected list row.
    pub fn selected_row(theme: &Theme, _status: button::Status) -> button::Style {
        let t = tokens(theme);
        button::Style { background: fill(t.selected), text_color: t.text, border: border::rounded(RADIUS + 2.0), ..button::Style::default() }
    }

    /// The row style for a selection state.
    /// Behind a hovered row's action icons: the row's own fill.
    pub fn row_actions(theme: &Theme, selected: bool) -> container::Style {
        let t = tokens(theme);
        container::Style { background: fill(if selected { t.selected } else { t.hover }), ..container::Style::default() }
    }

    pub fn row_for(selected: bool) -> fn(&Theme, button::Status) -> button::Style {
        if selected { selected_row } else { row }
    }

    /// The main action: a light fill with dark text (Allow once, Retry).
    pub fn primary_button(theme: &Theme, status: button::Status) -> button::Style {
        let t = tokens(theme);
        let bg = match status {
            button::Status::Hovered => t.text.scale_alpha(0.9),
            button::Status::Disabled => t.text.scale_alpha(0.4),
            _ => t.text,
        };
        button::Style { background: fill(bg), text_color: t.side, border: border::rounded(RADIUS + 2.0), ..button::Style::default() }
    }

    /// The accent action: send, start.
    pub fn accent_button(theme: &Theme, status: button::Status) -> button::Style {
        let t = tokens(theme);
        let bg = match status {
            button::Status::Hovered => t.accent.scale_alpha(0.9),
            button::Status::Disabled => t.accent.scale_alpha(0.4),
            _ => t.accent,
        };
        button::Style { background: fill(bg), text_color: t.on_accent, border: border::rounded(RADIUS + 3.0), ..button::Style::default() }
    }

    /// A secondary action: outlined, no fill.
    pub fn secondary_button(theme: &Theme, status: button::Status) -> button::Style {
        let t = tokens(theme);
        let background = matches!(status, button::Status::Hovered | button::Status::Pressed).then(|| Background::Color(t.hover));
        button::Style {
            background,
            text_color: t.text,
            border: Border { color: t.line_strong, width: 1.0, radius: (RADIUS + 2.0).into() },
            ..button::Style::default()
        }
    }

    /// A quiet text or icon button: secondary text, a hover fill.
    pub fn ghost_button(theme: &Theme, status: button::Status) -> button::Style {
        let t = tokens(theme);
        let (background, text_color) = match status {
            button::Status::Hovered | button::Status::Pressed => (fill(t.hover), t.text),
            _ => (None, t.text2),
        };
        button::Style { background, text_color, border: border::rounded(RADIUS), ..button::Style::default() }
    }

    /// A tab with an icon: a filled pill when active, secondary text
    /// otherwise.
    pub fn tab(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
        move |theme, status| {
            let t = tokens(theme);
            let (background, text_color) = match status {
                _ if active => (fill(t.selected), t.text),
                button::Status::Hovered | button::Status::Pressed => (fill(t.hover), t.text),
                button::Status::Active | button::Status::Disabled => (None, t.text2),
            };
            button::Style { background, text_color, border: border::rounded(RADIUS + 2.0), ..button::Style::default() }
        }
    }

    /// The line under the active tab. Kept for tab bars that still draw one.
    pub fn tab_underline(theme: &Theme) -> container::Style {
        container::Style { background: fill(tokens(theme).accent), ..container::Style::default() }
    }

    /// A small floating label: tooltips.
    pub fn tooltip(theme: &Theme) -> container::Style {
        let t = tokens(theme);
        container::Style {
            background: fill(t.raised),
            text_color: Some(t.text),
            border: Border { color: t.line_strong, width: 1.0, radius: RADIUS.into() },
            ..container::Style::default()
        }
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
    fn text_tokens_are_readable_on_their_grounds() {
        for tokens in [style::DARK, style::LIGHT] {
            for ground in [tokens.ground, tokens.side, tokens.card] {
                for (name, fg) in [("text", tokens.text), ("text2", tokens.text2), ("text3", tokens.text3), ("warn", tokens.warn)] {
                    assert!(contrast(ground, fg) >= 4.5, "{name} on {ground:?}: contrast {}", contrast(ground, fg));
                }
            }
            assert!(contrast(tokens.accent, tokens.on_accent) >= 4.5, "accent button contrast");
        }
    }

    #[test]
    fn waiting_badge_is_plain_text() {
        for theme in [dark(), light()] {
            let badge = style::waiting_badge(&theme);
            assert!(badge.background.is_none(), "{theme}: no pill fill");
            assert_eq!(badge.text_color, Some(style::tokens(&theme).warn));
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
