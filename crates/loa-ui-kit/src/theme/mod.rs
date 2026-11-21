//! The token layer: named roles, token sets, theme sources and loaders, the registry, and the
//! active set that style recipes read. A future theme script only produces data through
//! [`ThemeSource`]; widget code never changes.

mod builtin;
mod source;
#[cfg(test)]
mod tests;
mod tokens;

pub use builtin::BuiltinTheme;
pub use loa_config::{Appearance, ThemeRef};
pub use source::{ThemeLoader, ThemeRegistry, ThemeSource, pick_variant};
pub use tokens::{Look, Paint, StyleKey, StyleSpec, StyleStates, TokenSet, TokenValue};

use iced::{Color, Theme};
use std::sync::{Arc, LazyLock, RwLock};

/// Theme config overrides: a TOML table of dotted token keys.
pub type Overrides = toml::Table;

/// A role enum: `str_enum!` names plus the override prefix (`color`, `space`, ..).
pub trait Role: Copy + 'static {
    const KIND: &'static str;
    fn parse(s: &str) -> Option<Self>;
    fn index(self) -> usize;
}

/// Declares role enums with stable dotted names used by overrides and scripts.
macro_rules! roles {
    ($($kind:literal $name:ident { $($v:ident = $s:literal),+ $(,)? })+) => {$(
        loa_util::str_enum! { pub $name { $($v => $s),+ } }
        impl Role for $name { const KIND: &'static str = $kind; fn parse(s: &str) -> Option<Self> { Self::parse(s) } fn index(self) -> usize { self as usize } }
    )+};
}

roles! {
    "color" ColorRole {
        Ground = "ground", Side = "side", Card = "card", Raised = "raised", Hover = "hover", Selected = "selected",
        Line = "line", LineStrong = "line.strong", Text = "text", Text2 = "text.2", Text3 = "text.3", Accent = "accent",
        OnAccent = "on.accent", Ok = "ok", Warn = "warn", Danger = "danger", AddedBg = "diff.added", RemovedBg = "diff.removed",
        CursorRing = "cursor.ring", Scrim = "scrim", Shadow = "shadow",
        Term0 = "term.0", Term1 = "term.1", Term2 = "term.2", Term3 = "term.3", Term4 = "term.4", Term5 = "term.5",
        Term6 = "term.6", Term7 = "term.7", Term8 = "term.8", Term9 = "term.9", Term10 = "term.10", Term11 = "term.11",
        Term12 = "term.12", Term13 = "term.13", Term14 = "term.14", Term15 = "term.15",
    }
    "text" TextRole { Title = "title", Heading = "heading", Body = "body", Label = "label", Caption = "caption", Mono = "mono" }
    "space" SpaceRole { Xs = "xs", S = "s", M = "m", L = "l", Xl = "xl", Gutter = "gutter" }
    "radius" RadiusRole { Pill = "pill", Chip = "chip", Row = "row", Card = "card", Dot = "dot" }
    "alpha" AlphaRole { Muted = "muted", Disabled = "disabled", Tint = "tint", Hover = "hover" }
    "layout" LayoutRole {
        ComposerWidth = "composer.width", NavWidth = "nav.width", ListWidth = "list.width",
        PopupWidth = "popup.width", ContentWidth = "content.width", LabelColumn = "label.column",
    }
}

loa_util::str_enum! {
    /// Light or dark; a source offers one or both.
    pub Variant { Light => "light", Dark => "dark" }
}

loa_util::str_enum! {
    /// Interaction state a [`StyleSpec`] applies to.
    pub StyleState { Base => "base", Hover => "hover", Pressed => "pressed", Selected => "selected", Disabled => "disabled" }
}

impl ColorRole {
    /// The ANSI colour `n` (0..16) of the terminal.
    pub fn term(n: usize) -> Self { Self::ALL[Self::Term0 as usize + n.min(15)] }
}

/// Why a theme or an override could not be used.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ThemeError {
    #[error("unknown theme `{0}`")]
    UnknownTheme(String),
    #[error("unknown theme key `{0}`")]
    UnknownKey(String),
    #[error("bad value for `{0}`: {1}")]
    BadValue(String, String),
    #[error("theme `{0}` does not offer the {1} variant")]
    NoVariant(String, Variant),
    #[error("{0}")]
    Load(String),
}

static ACTIVE: LazyLock<RwLock<(u64, Arc<TokenSet>)>> = LazyLock::new(|| RwLock::new((0, builtin::tokens(Variant::Dark))));

/// The shipped token set of `v`.
pub fn builtin_tokens(v: Variant) -> Arc<TokenSet> { builtin::tokens(v) }

/// The active token set.
pub fn tokens() -> Arc<TokenSet> { ACTIVE.read().unwrap_or_else(|e| e.into_inner()).1.clone() }

/// The active `r` spacing.
pub fn sp(r: SpaceRole) -> f32 { tokens().space(r) }

/// Left and right padding of the active `r` spacing.
pub fn pad_x(r: SpaceRole) -> iced::Padding { iced::Padding::ZERO.left(sp(r)).right(sp(r)) }

/// Top and bottom padding of the active `r` spacing.
pub fn pad_y(r: SpaceRole) -> iced::Padding { iced::Padding::ZERO.top(sp(r)).bottom(sp(r)) }

/// The active `r` text size.
pub fn text_size(r: TextRole) -> f32 { tokens().text(r) }

/// The active `r` layout width.
pub fn width(r: LayoutRole) -> f32 { tokens().layout(r) }

/// Bumped by every [`set_active`]; caches compare it to know when to rebuild.
pub fn generation() -> u64 { ACTIVE.read().unwrap_or_else(|e| e.into_inner()).0 }

/// Makes `t` the active set and returns the new generation.
pub fn set_active(t: TokenSet) -> u64 {
    let mut active = ACTIVE.write().unwrap_or_else(|e| e.into_inner());
    *active = (active.0 + 1, Arc::new(t));
    active.0
}

/// The tokens behind `theme`: the active set when it built `theme`, else the builtin set of
/// the same lightness (tests and iced's stock themes).
pub fn for_theme(theme: &Theme) -> Arc<TokenSet> {
    let active = tokens();
    if active.palette() == theme.palette() { return active }
    builtin::tokens(if theme.extended_palette().is_dark { Variant::Dark } else { Variant::Light })
}

/// WCAG contrast ratio of two colours (1..=21).
pub fn contrast(a: Color, b: Color) -> f32 {
    let lum = |c: Color| {
        let ch = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b)
    };
    let (la, lb) = (lum(a), lum(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}
