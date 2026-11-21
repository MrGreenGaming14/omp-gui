//! Style recipes. Each reads its [`StyleStates`](crate::theme::StyleStates) or roles from the
//! active tokens at draw time, so overrides and future theme scripts restyle every widget.

use crate::theme::{ColorRole as C, StyleKey, StyleState, TokenSet, for_theme};
use iced::widget::{self as w, pick_list, scrollable, svg, text_input};
use iced::{Background, Border, Color, Shadow, Theme, Vector, border};
use loa_util::Semantic;

/// Button looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Accent,
    Secondary,
    Ghost,
    Row { selected: bool },
    Tab { active: bool },
    Flat,
    Danger,
}

impl ButtonKind {
    pub fn key(self) -> StyleKey {
        let part = match self {
            Self::Primary => "primary",
            Self::Accent => "accent",
            Self::Secondary => "secondary",
            Self::Ghost => "ghost",
            Self::Row { .. } => "row",
            Self::Tab { .. } => "tab",
            Self::Flat => "flat",
            Self::Danger => "danger",
        };
        StyleKey { component: "button", part }
    }
}

loa_util::str_enum! {
    /// Container looks; the name is the recipe's `container.<name>` key.
    pub Surface {
        Ground => "ground", Side => "side", Card => "card", Attention => "attention", Raised => "raised", Popup => "popup",
        Tooltip => "tooltip", Well => "well", ErrorBox => "error", Badge => "badge", Scrim => "scrim",
    }
}

loa_util::str_enum! {
    /// Text and icon colours by meaning.
    pub Tone { Normal => "normal", Muted => "muted", Faint => "faint", Ok => "ok", Warn => "warn", Danger => "danger", Accent => "accent", OnAccent => "on.accent" }
}

impl Tone {
    pub fn role(self) -> C {
        match self {
            Self::Normal => C::Text,
            Self::Muted => C::Text2,
            Self::Faint => C::Text3,
            Self::Ok => C::Ok,
            Self::Warn => C::Warn,
            Self::Danger => C::Danger,
            Self::Accent => C::Accent,
            Self::OnAccent => C::OnAccent,
        }
    }

    /// This tone's colour in `theme`.
    pub fn color(self, theme: &Theme) -> Color { for_theme(theme).color(self.role()) }
}

/// The tone of a domain status.
pub fn semantic(s: Semantic) -> Tone {
    match s {
        Semantic::Ok => Tone::Ok,
        Semantic::Warn => Tone::Warn,
        Semantic::Error => Tone::Danger,
        Semantic::Muted => Tone::Faint,
        Semantic::Busy | Semantic::Accent => Tone::Accent,
        Semantic::Neutral => Tone::Normal,
    }
}

/// `weak` while hovered, `strong` while pressed, nothing otherwise.
pub fn state_bg(status: w::button::Status, weak: Color, strong: Color) -> Option<Background> {
    match status {
        w::button::Status::Hovered => Some(weak.into()),
        w::button::Status::Pressed => Some(strong.into()),
        _ => None,
    }
}

pub fn button(k: ButtonKind) -> impl Fn(&Theme, w::button::Status) -> w::button::Style {
    move |theme, status| {
        let t = for_theme(theme);
        let state = match (k, status) {
            (ButtonKind::Row { selected: true } | ButtonKind::Tab { active: true }, _) => StyleState::Selected,
            (_, w::button::Status::Hovered) => StyleState::Hover,
            (_, w::button::Status::Pressed) => StyleState::Pressed,
            (_, w::button::Status::Disabled) => StyleState::Disabled,
            (_, w::button::Status::Active) => StyleState::Base,
        };
        let look = t.look(k.key(), state);
        let text_color = look.fg.unwrap_or(t.color(C::Text));
        w::button::Style { background: look.bg.map(Background::Color), text_color, border: look.border, ..w::button::Style::default() }
    }
}

pub fn container(s: Surface) -> impl Fn(&Theme) -> w::container::Style { move |theme| container_in(&for_theme(theme), s) }

/// [`container`] against a given token set.
pub fn container_in(t: &TokenSet, s: Surface) -> w::container::Style {
    let look = t.look(StyleKey { component: "container", part: s.as_str() }, StyleState::Base);
    let shadow = match s {
        Surface::Popup => Shadow { color: t.color(C::Shadow), offset: Vector::new(0.0, 16.0), blur_radius: 40.0 },
        _ => Shadow::default(),
    };
    w::container::Style { background: look.bg.map(Background::Color), text_color: look.fg, border: look.border, shadow, ..w::container::Style::default() }
}

/// A solid fill of `role`, rounded by `radius` (dots, meter bars, underlines).
pub fn fill(role: C, radius: f32) -> impl Fn(&Theme) -> w::container::Style {
    move |theme| w::container::Style { background: Some(for_theme(theme).color(role).into()), border: border::rounded(radius), ..w::container::Style::default() }
}

pub fn text(t: Tone) -> impl Fn(&Theme) -> w::text::Style { move |theme| w::text::Style { color: Some(t.color(theme)) } }

pub fn tint(t: Tone) -> impl Fn(&Theme, svg::Status) -> svg::Style { move |theme, _| svg::Style { color: Some(t.color(theme)) } }

pub fn input(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let t = for_theme(theme);
    let line = match status {
        text_input::Status::Focused { .. } => C::Accent,
        text_input::Status::Hovered => C::LineStrong,
        text_input::Status::Active | text_input::Status::Disabled => C::Line,
    };
    text_input::Style {
        background: t.color(C::Side).into(),
        border: Border { color: t.color(line), width: 1.0, radius: t.radius(crate::theme::RadiusRole::Chip).into() },
        icon: t.color(C::Text3),
        placeholder: t.color(C::Text3),
        value: t.color(C::Text),
        selection: t.color(C::Accent).scale_alpha(t.alpha(crate::theme::AlphaRole::Tint)),
    }
}

pub fn pick(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let t = for_theme(theme);
    let line = match status {
        pick_list::Status::Opened { .. } => C::Accent,
        pick_list::Status::Hovered => C::LineStrong,
        pick_list::Status::Active => C::Line,
    };
    pick_list::Style {
        text_color: t.color(C::Text),
        placeholder_color: t.color(C::Text3),
        handle_color: t.color(C::Text2),
        background: t.color(C::Side).into(),
        border: Border { color: t.color(line), width: 1.0, radius: t.radius(crate::theme::RadiusRole::Chip).into() },
    }
}

/// A rail-less scroller in the line tone, stronger while hovered or dragged; `thin` for tab bars.
pub fn scrollbar(thin: bool) -> impl Fn(&Theme, scrollable::Status) -> scrollable::Style {
    move |theme, status| {
        let t = for_theme(theme);
        let role = match status {
            scrollable::Status::Dragged { .. } | scrollable::Status::Hovered { .. } => C::Text3,
            scrollable::Status::Active { .. } => C::LineStrong,
        };
        let scroller = scrollable::Scroller { background: t.color(role).into(), border: border::rounded(if thin { 1 } else { 4 }) };
        let rail = scrollable::Rail { background: None, border: Border::default(), scroller };
        scrollable::Style { vertical_rail: rail, horizontal_rail: rail, ..scrollable::default(theme, status) }
    }
}

/// A 1px line between areas.
pub fn rule(theme: &Theme) -> w::rule::Style { w::rule::Style { color: for_theme(theme).color(C::Line), ..w::rule::default(theme) } }
