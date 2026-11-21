//! The shipped themes: warm neutrals with one sage accent, dark and light, defined once.

use super::tokens::{Paint, StyleKey, StyleSpec, StyleStates, TokenSet};
use super::{AlphaRole, ColorRole as C, RadiusRole, StyleState, ThemeError, ThemeSource, Variant};
use iced::{Color, Font, color};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

/// Colours in [`C`] order; the terminal colours are the ANSI 16.
const DARK: [Color; C::COUNT] = [
    color!(0x262624), color!(0x1f1e1d), color!(0x2b2a28), color!(0x30302e), color!(0x2b2a28), color!(0x34332f),
    color!(0x33322f), color!(0x4a4945), color!(0xecebe6), color!(0xb7b5ac), color!(0x9a988f), color!(0x7fb7a4),
    color!(0x1f1e1d), color!(0x6fbf73), color!(0xf0b866), color!(0xe8a0a0), color!(0x6fbf73, 0.14), color!(0xe8a0a0, 0.14),
    color!(0x7fb7a4), Color { r: 0.055, g: 0.051, b: 0.047, a: 0.6 }, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.45 },
    color!(0x3a3a37), color!(0xe8a0a0), color!(0x6fbf73), color!(0xf0b866), color!(0x6fa0bf), color!(0xaca0b0),
    color!(0x6fb099), color!(0xbabab6), color!(0x7f7f7b), color!(0xe9b6b5), color!(0x94cc95), color!(0xefc78c),
    color!(0x94b7cb), color!(0xbfb7c0), color!(0x94c1b0), color!(0xecebe6),
];

const LIGHT: [Color; C::COUNT] = [
    color!(0xfaf9f5), color!(0xf1efe8), color!(0xffffff), color!(0xffffff), color!(0xebe8df), color!(0xe4e0d5),
    color!(0xe3e0d6), color!(0xcbc7bb), color!(0x1f1e1d), color!(0x55534d), color!(0x6f6d66), color!(0x2f7a66),
    color!(0xffffff), color!(0x2f8f5b), color!(0x8a560c), color!(0xb3423a), color!(0x2f8f5b, 0.12), color!(0xb3423a, 0.12),
    color!(0x2f7a66), Color { r: 0.1, g: 0.1, b: 0.09, a: 0.35 }, Color { r: 0.0, g: 0.0, b: 0.0, a: 0.18 },
    color!(0x353433), color!(0xb3423a), color!(0x2f8f5b), color!(0x8a560c), color!(0x2f438f), color!(0x714364),
    color!(0x2f6975), color!(0xc3c2bf), color!(0x82817e), color!(0xc87972), color!(0x6caf89), color!(0xac8752),
    color!(0x6c7aae), color!(0x9a7990), color!(0x6c949b), color!(0xfaf9f5),
];

fn set(variant: Variant) -> TokenSet {
    let (name, colors) = match variant { Variant::Dark => ("Loa Dark", DARK), Variant::Light => ("Loa Light", LIGHT) };
    TokenSet {
        name: name.into(),
        variant,
        colors,
        text: [20.0, 16.0, 14.0, 13.0, 12.0, 12.0].map(|s| (s, Font::DEFAULT)),
        space: [4.0, 8.0, 12.0, 16.0, 24.0, 24.0],
        radii: [4.0, 6.0, 8.0, 12.0, 99.0],
        alphas: [0.55, 0.4, 0.35, 0.9],
        layout: [700.0, 272.0, 380.0, 520.0, 760.0, 160.0, 190.0, 1440.0, 900.0, 900.0, 640.0, 660.0, 640.0, 560.0, 780.0, 360.0, 280.0],
        styles: styles(),
    }
    .with_mono()
}

impl TokenSet {
    fn with_mono(mut self) -> Self {
        self.text[super::TextRole::Mono as usize].1 = Font::MONOSPACE;
        self
    }
}

fn spec(bg: Option<Paint>, fg: Option<C>, border: Option<Paint>, radius: Option<RadiusRole>) -> Option<StyleSpec> {
    Some(StyleSpec { bg, fg: fg.map(Paint::Role), border: border.map(|p| (p, 1.0)), radius })
}

/// The default recipes; overrides and scripts edit these by `component.part.state.field`.
fn styles() -> HashMap<StyleKey, StyleStates> {
    use Paint::{Alpha, Role};
    use RadiusRole::{Card, Chip, Row};
    let filled = |bg: C, fg: C, r| {
        let mut s = [None; StyleState::COUNT];
        s[0] = spec(Some(Role(bg)), Some(fg), None, Some(r));
        s[StyleState::Hover as usize] = spec(Some(Alpha(bg, AlphaRole::Hover)), None, None, None);
        s[StyleState::Disabled as usize] = spec(Some(Alpha(bg, AlphaRole::Disabled)), None, None, None);
        s
    };
    let hovered = |base: Option<StyleSpec>, hover: Option<StyleSpec>, selected: Option<StyleSpec>| [base, hover, None, selected, None];
    let plain = |bg: Option<C>, fg: Option<C>, border: Option<Paint>, r| [spec(bg.map(Role), fg, border, r), None, None, None, None];
    let hover = spec(Some(Role(C::Hover)), Some(C::Text), None, None);
    let selected = spec(Some(Role(C::Selected)), Some(C::Text), None, None);
    [
        ("button", "primary", filled(C::Text, C::Side, Row)),
        ("button", "accent", filled(C::Accent, C::OnAccent, Row)),
        ("button", "danger", filled(C::Danger, C::OnAccent, Row)),
        ("button", "secondary", hovered(spec(None, Some(C::Text), Some(Role(C::LineStrong)), Some(Row)), hover, None)),
        ("button", "ghost", hovered(spec(None, Some(C::Text2), None, Some(Chip)), hover, None)),
        ("button", "row", [spec(None, Some(C::Text), None, Some(Row)), hover, selected, selected, None]),
        ("button", "tab", hovered(spec(None, Some(C::Text2), None, Some(Row)), hover, selected)),
        ("button", "flat", plain(None, Some(C::Text), None, Some(Chip))),
        ("container", "ground", plain(Some(C::Ground), Some(C::Text), None, None)),
        ("container", "side", plain(Some(C::Side), Some(C::Text), None, None)),
        ("container", "card", plain(Some(C::Card), Some(C::Text), Some(Role(C::Line)), Some(Card))),
        ("container", "attention", plain(Some(C::Card), Some(C::Text), Some(Alpha(C::Warn, AlphaRole::Tint)), Some(Card))),
        ("container", "raised", plain(Some(C::Raised), Some(C::Text), Some(Role(C::Line)), Some(Card))),
        ("container", "popup", plain(Some(C::Card), Some(C::Text), Some(Role(C::LineStrong)), Some(Card))),
        ("container", "tooltip", plain(Some(C::Raised), Some(C::Text), Some(Role(C::LineStrong)), Some(Chip))),
        ("container", "well", plain(Some(C::Ground), Some(C::Text), Some(Role(C::LineStrong)), Some(Chip))),
        ("container", "error", [spec(Some(Alpha(C::Danger, AlphaRole::Tint)), Some(C::Text), Some(Role(C::Danger)), Some(Chip)), None, None, None, None]),
        ("container", "badge", plain(None, Some(C::Text3), None, None)),
        ("container", "scrim", plain(Some(C::Scrim), None, None, None)),
    ]
    .into_iter()
    .map(|(component, part, states)| (StyleKey { component, part }, StyleStates(states)))
    .collect()
}

static SETS: LazyLock<[Arc<TokenSet>; 2]> = LazyLock::new(|| [Arc::new(set(Variant::Light)), Arc::new(set(Variant::Dark))]);

/// The builtin token set of `v`.
pub(super) fn tokens(v: Variant) -> Arc<TokenSet> { SETS[v as usize].clone() }

/// A shipped theme: `system` offers both variants, `dark` and `light` one each.
pub struct BuiltinTheme {
    id: &'static str,
    label: &'static str,
    variants: &'static [Variant],
}

impl BuiltinTheme {
    pub const ALL: [BuiltinTheme; 3] = [
        BuiltinTheme { id: "system", label: "System", variants: &[Variant::Dark, Variant::Light] },
        BuiltinTheme { id: "dark", label: "Dark", variants: &[Variant::Dark] },
        BuiltinTheme { id: "light", label: "Light", variants: &[Variant::Light] },
    ];
}

impl ThemeSource for BuiltinTheme {
    fn id(&self) -> &str { self.id }
    fn label(&self) -> &str { self.label }
    fn variants(&self) -> &[Variant] { self.variants }
    fn tokens(&self, v: Variant) -> Result<TokenSet, ThemeError> {
        if !self.variants.contains(&v) { return Err(ThemeError::NoVariant(self.id.into(), v)) }
        Ok((*tokens(v)).clone())
    }
}
