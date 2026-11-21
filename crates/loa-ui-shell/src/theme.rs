//! The active theme: the saved [`Appearance`] resolved through the kit's [`ThemeRegistry`]
//! against the system's light or dark mode. Re-exports the kit token layer.

pub use loa_ui_kit::theme::*;

use iced::theme::Mode;
use iced::Theme;

#[derive(Clone)]
pub struct State {
    registry: std::sync::Arc<ThemeRegistry>,
    appearance: Appearance,
    system: Mode,
    theme: Theme,
    variant: Variant,
    errors: Vec<ThemeError>,
}

impl State {
    pub fn new(appearance: Appearance) -> Self {
        let mut s = Self { registry: ThemeRegistry::builtin().into(), appearance, system: Mode::None, theme: Theme::Dark, variant: Variant::Dark, errors: vec![] };
        s.refresh();
        s
    }
    pub fn preference(&self) -> &ThemeRef { &self.appearance.theme }
    pub fn themes(&self) -> Vec<(String, String)> { self.registry.list() }
    pub fn errors(&self) -> &[ThemeError] { &self.errors }
    /// An unknown system mode counts as dark.
    pub fn variant(&self) -> Variant { self.variant }
    pub fn theme(&self) -> Theme { self.theme.clone() }
    /// Picks a theme; returns the new token generation.
    pub fn set_preference(&mut self, theme: ThemeRef) -> u64 {
        self.appearance.theme = theme;
        self.refresh()
    }
    pub fn set_system_mode(&mut self, mode: Mode) -> u64 {
        self.system = mode;
        self.refresh()
    }
    fn refresh(&mut self) -> u64 {
        let preferred = if self.system == Mode::Light { Variant::Light } else { Variant::Dark };
        let (tokens, errors) = self.registry.tokens(&self.appearance, preferred);
        (self.theme, self.variant, self.errors) = (tokens.theme(), tokens.variant, errors);
        set_active(tokens)
    }
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("State").field("appearance", &self.appearance).finish_non_exhaustive() }
}

/// Legacy style functions over the kit recipes for loa-app and loa-ui-surface until P28/P30 port them.
pub mod style {
    use iced::widget::{button, container, rule, text, text_input};
    use iced::{Background, Color, Theme};
    use loa_ui_kit::style::{self as kit, ButtonKind as B, Surface as S, Tone};
    use loa_ui_kit::theme::{ColorRole as C, RadiusRole, for_theme};

    macro_rules! recipes {
        ($($name:ident: $kind:ident = $recipe:expr;)*) => { $(recipes!(@ $kind $name $recipe);)* };
        (@ container $name:ident $recipe:expr) => { pub fn $name(t: &Theme) -> container::Style { $recipe(t) } };
        (@ text $name:ident $recipe:expr) => { pub fn $name(t: &Theme) -> text::Style { $recipe(t) } };
        (@ button $name:ident $recipe:expr) => { pub fn $name(t: &Theme, s: button::Status) -> button::Style { $recipe(t, s) } };
    }
    recipes! {
        surface: container = kit::container(S::Ground); side: container = kit::container(S::Side); card: container = kit::container(S::Card);
        window: container = kit::container(S::Popup); scrim: container = kit::container(S::Scrim); badge: container = kit::container(S::Badge);
        muted: text = kit::text(Tone::Muted); faint: text = kit::text(Tone::Faint); waiting_text: text = kit::text(Tone::Warn);
        row: button = kit::button(B::Row { selected: false }); primary_button: button = kit::button(B::Primary);
        secondary_button: button = kit::button(B::Secondary); ghost_button: button = kit::button(B::Ghost);
    }
    pub const RADIUS: f32 = 6.0;
    pub fn border_color(t: &Theme) -> Color { for_theme(t).color(C::Line) }
    pub fn divider(t: &Theme) -> rule::Style { kit::rule(t) }
    pub fn input(t: &Theme, s: text_input::Status) -> text_input::Style { kit::input(t, s) }
    pub fn muted_color(t: &Theme) -> Color { Tone::Muted.color(t) }
    pub fn row_for(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style { kit::button(B::Row { selected }) }
    pub fn tab(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style { kit::button(B::Tab { active }) }
    pub fn meter_track(t: &Theme) -> container::Style { kit::fill(C::LineStrong, for_theme(t).radius(RadiusRole::Dot))(t) }
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
