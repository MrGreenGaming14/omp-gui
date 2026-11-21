//! The active theme: the saved [`Appearance`] resolved through the kit's [`ThemeRegistry`]
//! against the system's light or dark mode. Re-exports the kit token layer.

pub use loa_ui_kit::theme::*;

use iced::theme::Mode;
use iced::{Subscription, Task, Theme};

/// The builtin dark theme.
pub fn dark() -> Theme { builtin(Variant::Dark) }

/// The builtin light theme.
pub fn light() -> Theme { builtin(Variant::Light) }

fn builtin(v: Variant) -> Theme { builtin_tokens(v).theme() }

/// The appearance the user picked, the system mode it may follow, and the resolved theme.
#[derive(Clone)]
pub struct State {
    registry: std::sync::Arc<ThemeRegistry>,
    appearance: Appearance,
    system: Mode,
    theme: Theme,
    generation: u64,
    errors: Vec<ThemeError>,
}

impl State {
    pub fn new(appearance: Appearance) -> Self { Self::with_registry(ThemeRegistry::builtin(), appearance) }

    /// Resolves through `registry`, which may carry extra sources and loaders.
    pub fn with_registry(registry: ThemeRegistry, appearance: Appearance) -> Self {
        let mut s = Self { registry: registry.into(), appearance, system: Mode::None, theme: Theme::Dark, generation: 0, errors: vec![] };
        s.refresh();
        s
    }

    /// Asks the system for its mode; feed the result to [`State::set_system_mode`].
    pub fn detect() -> Task<Mode> { iced::system::theme() }

    pub fn subscription() -> Subscription<Mode> { iced::system::theme_changes() }

    pub fn preference(&self) -> &ThemeRef { &self.appearance.theme }

    pub fn appearance(&self) -> &Appearance { &self.appearance }

    /// `(id, label)` of every theme the registry offers.
    pub fn themes(&self) -> Vec<(String, String)> { self.registry.list() }

    /// The generation of the active token set; bumps on every change.
    pub fn generation(&self) -> u64 { self.generation }

    /// What failed in the last resolve: an unknown theme or bad overrides.
    pub fn errors(&self) -> &[ThemeError] { &self.errors }

    /// Picks a theme by ref; returns the new generation.
    pub fn set_preference(&mut self, theme: ThemeRef) -> u64 {
        self.appearance.theme = theme;
        self.refresh()
    }

    /// Replaces the whole appearance, overrides included.
    pub fn set_appearance(&mut self, appearance: Appearance) -> u64 {
        self.appearance = appearance;
        self.refresh()
    }

    pub fn set_system_mode(&mut self, mode: Mode) -> u64 {
        self.system = mode;
        self.refresh()
    }

    /// The variant in effect; an unknown system mode counts as dark.
    pub fn variant(&self) -> Variant { if self.theme.extended_palette().is_dark { Variant::Dark } else { Variant::Light } }

    /// The `iced` theme to return from the application's `theme` function.
    pub fn theme(&self) -> Theme { self.theme.clone() }

    fn refresh(&mut self) -> u64 {
        let preferred = if self.system == Mode::Light { Variant::Light } else { Variant::Dark };
        let (tokens, errors) = self.registry.tokens(&self.appearance, preferred);
        self.theme = tokens.theme();
        self.errors = errors;
        self.generation = set_active(tokens);
        self.generation
    }
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State").field("appearance", &self.appearance).field("generation", &self.generation).finish_non_exhaustive()
    }
}

impl Default for State {
    fn default() -> Self { Self::new(Appearance::default()) }
}

/// Legacy style functions over the kit recipes, kept until each crate is ported to the kit.
pub mod style {
    use loa_ui_kit::style::{self as kit, ButtonKind as B, Surface as S, Tone};
    use loa_ui_kit::theme::{ColorRole as C, RadiusRole, for_theme};
    use iced::widget::{button, container, rule, scrollable, text, text_input};
    use iced::{Background, Color, Theme};

    pub const RADIUS: f32 = 6.0;
    pub const CARD_RADIUS: f32 = 12.0;

    /// The colours of one theme by their former field names.
    #[derive(Debug, Clone, Copy)]
    pub struct Tokens {
        pub ground: Color, pub side: Color, pub card: Color, pub raised: Color, pub hover: Color, pub selected: Color,
        pub line: Color, pub line_strong: Color, pub text: Color, pub text2: Color, pub text3: Color, pub accent: Color,
        pub on_accent: Color, pub warn: Color, pub ok: Color, pub danger: Color, pub scrim: Color,
    }

    pub fn tokens(theme: &Theme) -> Tokens {
        let t = for_theme(theme);
        let c = |r| t.color(r);
        Tokens {
            ground: c(C::Ground), side: c(C::Side), card: c(C::Card), raised: c(C::Raised), hover: c(C::Hover), selected: c(C::Selected),
            line: c(C::Line), line_strong: c(C::LineStrong), text: c(C::Text), text2: c(C::Text2), text3: c(C::Text3), accent: c(C::Accent),
            on_accent: c(C::OnAccent), warn: c(C::Warn), ok: c(C::Ok), danger: c(C::Danger), scrim: c(C::Scrim),
        }
    }

    pub fn surface(t: &Theme) -> container::Style { kit::container(S::Ground)(t) }
    pub fn chrome(t: &Theme) -> container::Style { surface(t) }
    pub fn side(t: &Theme) -> container::Style { kit::container(S::Side)(t) }
    pub fn card(t: &Theme) -> container::Style { kit::container(S::Card)(t) }
    pub fn window(t: &Theme) -> container::Style { kit::container(S::Popup)(t) }
    pub fn scrim(t: &Theme) -> container::Style { kit::container(S::Scrim)(t) }
    pub fn badge(t: &Theme) -> container::Style { kit::container(S::Badge)(t) }
    pub fn tooltip(t: &Theme) -> container::Style { kit::container(S::Tooltip)(t) }
    pub fn waiting_badge(t: &Theme) -> container::Style { container::Style { text_color: Some(Tone::Warn.color(t)), ..Default::default() } }
    pub fn border_color(t: &Theme) -> Color { for_theme(t).color(C::Line) }
    pub fn divider(t: &Theme) -> rule::Style { kit::rule(t) }
    pub fn tab_scrollbar(t: &Theme, s: scrollable::Status) -> scrollable::Style { kit::scrollbar(true)(t, s) }
    pub fn input(t: &Theme, s: text_input::Status) -> text_input::Style { kit::input(t, s) }
    pub fn muted(t: &Theme) -> text::Style { kit::text(Tone::Muted)(t) }
    pub fn faint(t: &Theme) -> text::Style { kit::text(Tone::Faint)(t) }
    pub fn waiting_text(t: &Theme) -> text::Style { kit::text(Tone::Warn)(t) }
    pub fn muted_color(t: &Theme) -> Color { Tone::Muted.color(t) }
    pub fn row(t: &Theme, s: button::Status) -> button::Style { kit::button(B::Row { selected: false })(t, s) }
    pub fn selected_row(t: &Theme, s: button::Status) -> button::Style { kit::button(B::Row { selected: true })(t, s) }
    pub fn row_for(selected: bool) -> fn(&Theme, button::Status) -> button::Style { if selected { selected_row } else { row } }
    pub fn primary_button(t: &Theme, s: button::Status) -> button::Style { kit::button(B::Primary)(t, s) }
    pub fn secondary_button(t: &Theme, s: button::Status) -> button::Style { kit::button(B::Secondary)(t, s) }
    pub fn ghost_button(t: &Theme, s: button::Status) -> button::Style { kit::button(B::Ghost)(t, s) }
    pub fn tab(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style { kit::button(B::Tab { active }) }
    pub fn tab_underline(t: &Theme) -> container::Style { kit::fill(C::Accent, 0.0)(t) }
    pub fn meter_track(t: &Theme) -> container::Style { kit::fill(C::LineStrong, for_theme(t).radius(RadiusRole::Dot))(t) }

    /// Behind a hovered row's action icons: the row's own fill.
    pub fn row_actions(t: &Theme, selected: bool) -> container::Style { kit::fill(if selected { C::Selected } else { C::Hover }, 0.0)(t) }

    /// A dot: green when online, faint when not.
    pub fn status_dot(online: bool) -> impl Fn(&Theme) -> container::Style { kit::fill(if online { C::Ok } else { C::Text3 }, 99.0) }

    /// The filled part of a meter: accent below half, warn from half, danger from 80%.
    pub fn meter_fill(fraction: f32) -> impl Fn(&Theme) -> container::Style {
        kit::fill(if fraction >= 0.8 { C::Danger } else if fraction >= 0.5 { C::Warn } else { C::Accent }, 99.0)
    }

    /// Under the floating composer: clear at the top, fading into the ground.
    pub fn composer_fade(theme: &Theme) -> container::Style {
        let g = for_theme(theme).color(C::Ground);
        let fade = iced::gradient::Linear::new(iced::Degrees(180.0)).add_stop(0.0, g.scale_alpha(0.0)).add_stop(0.14, g.scale_alpha(0.97)).add_stop(1.0, g);
        container::Style { background: Some(Background::Gradient(fade.into())), ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_follows_the_mode_an_explicit_theme_wins_and_generations_advance() {
        let named = |s: &str| Appearance { theme: ThemeRef(s.into()), ..Default::default() };
        let mut state = State::new(Appearance::default());
        assert_eq!(state.variant(), Variant::Dark, "unknown system mode falls back to dark");
        let g = state.set_system_mode(Mode::Light);
        assert_eq!((state.variant(), state.theme().palette()), (Variant::Light, light().palette()));
        assert!(state.set_preference(ThemeRef("dark".into())) > g);
        assert_eq!(state.variant(), Variant::Dark);
        state.set_appearance(named("nope"));
        assert_eq!((state.variant(), state.errors().len()), (Variant::Light, 1), "unknown themes fall back to system");
        let mut accent = named("light");
        accent.overrides.insert("color.accent".into(), "#ff0000".into());
        state.set_appearance(accent);
        assert_eq!(state.theme().palette().primary, iced::Color::from_rgb8(255, 0, 0));
    }
}
