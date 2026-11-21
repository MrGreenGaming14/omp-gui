//! [`TokenSet`]: every visual value of one theme variant, indexed by role, plus the
//! data-driven widget recipes, and the override parser that edits both by dotted key.

use super::{AlphaRole, ColorRole, LayoutRole, RadiusRole, Role, SpaceRole, StyleState, TextRole, ThemeError, Variant};
use iced::theme::Palette;
use iced::{Border, Color, Font, Theme};
use std::collections::HashMap;

/// A colour reference resolved against the tokens at draw time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    Role(ColorRole),
    Alpha(ColorRole, AlphaRole),
    Scaled(ColorRole, f32),
    Literal(Color),
}

/// One widget recipe: `component.part`, as in `button.ghost` or `container.card`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StyleKey {
    pub component: &'static str,
    pub part: &'static str,
}

/// The visual fields a recipe sets; `None` inherits from the base state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StyleSpec {
    pub bg: Option<Paint>,
    pub fg: Option<Paint>,
    pub border: Option<(Paint, f32)>,
    pub radius: Option<RadiusRole>,
}

/// A recipe per [`StyleState`]; `Pressed` falls back to `Hover`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StyleStates(pub [Option<StyleSpec>; StyleState::COUNT]);

/// A recipe resolved to colours.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Look {
    pub bg: Option<Color>,
    pub fg: Option<Color>,
    pub border: Border,
}

/// An override value: `#rrggbb[aa]`, a number, another role's name, or `role @ alpha`.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenValue {
    Color(Color),
    Number(f32),
    Ref(String),
    Alpha { base: String, alpha: String },
}

/// Every visual value of one theme variant.
#[derive(Clone, Debug, PartialEq)]
pub struct TokenSet {
    pub name: String,
    pub variant: Variant,
    pub colors: [Color; ColorRole::COUNT],
    pub text: [(f32, Font); TextRole::COUNT],
    pub space: [f32; SpaceRole::COUNT],
    pub radii: [f32; RadiusRole::COUNT],
    pub alphas: [f32; AlphaRole::COUNT],
    pub layout: [f32; LayoutRole::COUNT],
    pub styles: HashMap<StyleKey, StyleStates>,
}

impl TokenSet {
    pub fn color(&self, r: ColorRole) -> Color { self.colors[r as usize] }
    pub fn text(&self, r: TextRole) -> f32 { self.text[r as usize].0 }
    pub fn font(&self, r: TextRole) -> Font { self.text[r as usize].1 }
    pub fn space(&self, r: SpaceRole) -> f32 { self.space[r as usize] }
    pub fn radius(&self, r: RadiusRole) -> f32 { self.radii[r as usize] }
    pub fn alpha(&self, r: AlphaRole) -> f32 { self.alphas[r as usize] }
    pub fn layout(&self, r: LayoutRole) -> f32 { self.layout[r as usize] }
    pub fn is_dark(&self) -> bool { self.variant == Variant::Dark }

    pub fn paint(&self, p: Paint) -> Color {
        match p {
            Paint::Role(r) => self.color(r),
            Paint::Alpha(r, a) => self.color(r).scale_alpha(self.alpha(a)),
            Paint::Scaled(r, a) => self.color(r).scale_alpha(a),
            Paint::Literal(c) => c,
        }
    }

    /// The iced palette, derived from the colour roles.
    pub fn palette(&self) -> Palette {
        let c = |r| self.color(r);
        Palette {
            background: c(ColorRole::Ground),
            text: c(ColorRole::Text),
            primary: c(ColorRole::Accent),
            success: c(ColorRole::Ok),
            warning: c(ColorRole::Warn),
            danger: c(ColorRole::Danger),
        }
    }

    /// An iced theme carrying [`TokenSet::palette`].
    pub fn theme(&self) -> Theme { Theme::custom(self.name.clone(), self.palette()) }

    /// The recipe `key` in `state`, layered over its base state.
    pub fn look(&self, key: StyleKey, state: StyleState) -> Look {
        let states = self.styles.get(&key).copied().unwrap_or_default().0;
        let base = states[0].unwrap_or_default();
        let over = states[state as usize].or(if state == StyleState::Pressed { states[StyleState::Hover as usize] } else { None });
        let s = over.map_or(base, |o| StyleSpec {
            bg: o.bg.or(base.bg),
            fg: o.fg.or(base.fg),
            border: o.border.or(base.border),
            radius: o.radius.or(base.radius),
        });
        let (bc, bw) = s.border.map_or((Color::TRANSPARENT, 0.0), |(p, w)| (self.paint(p), w));
        let radius = s.radius.map_or(0.0, |r| self.radius(r));
        Look { bg: s.bg.map(|p| self.paint(p)), fg: s.fg.map(|p| self.paint(p)), border: Border { color: bc, width: bw, radius: radius.into() } }
    }

    /// Applies one override such as `color.accent` or `style.button.ghost.hover.bg`.
    pub fn apply(&mut self, key: &str, v: TokenValue) -> Result<(), ThemeError> {
        let bad = |why: &str| ThemeError::BadValue(key.to_owned(), why.to_owned());
        let unknown = || ThemeError::UnknownKey(key.to_owned());
        let (kind, name) = key.split_once('.').ok_or_else(unknown)?;
        let num = |v: &TokenValue| match v { TokenValue::Number(n) => Ok(*n), _ => Err(bad("expected a number")) };
        fn slot<'a, R: Role>(name: &str, arr: &'a mut [f32]) -> Option<&'a mut f32> { R::parse(name).map(|r| &mut arr[r.index()]) }
        let target = match kind {
            "color" => {
                let r = ColorRole::parse(name).ok_or_else(unknown)?;
                self.colors[r as usize] = self.paint(self.to_paint(&v).ok_or_else(|| bad("expected a colour"))?);
                return Ok(());
            }
            "text" => TextRole::parse(name).map(|r| &mut self.text[r as usize].0),
            "space" => slot::<SpaceRole>(name, &mut self.space),
            "radius" => slot::<RadiusRole>(name, &mut self.radii),
            "alpha" => slot::<AlphaRole>(name, &mut self.alphas),
            "layout" => slot::<LayoutRole>(name, &mut self.layout),
            "style" => return self.apply_style(name, &v).ok_or_else(|| bad("unknown recipe or value")),
            _ => None,
        };
        *target.ok_or_else(unknown)? = num(&v)?;
        Ok(())
    }

    fn apply_style(&mut self, path: &str, v: &TokenValue) -> Option<()> {
        let (rest, field) = path.rsplit_once('.')?;
        let (key, state) = rest.rsplit_once('.')?;
        let state = StyleState::parse(state)?;
        let key = *self.styles.keys().find(|k| format!("{}.{}", k.component, k.part) == key)?;
        let paint = self.to_paint(v);
        let radius = match v { TokenValue::Ref(r) => RadiusRole::parse(r), _ => None };
        let spec = self.styles.get_mut(&key)?.0[state as usize].get_or_insert_default();
        match field {
            "bg" => spec.bg = Some(paint?),
            "fg" => spec.fg = Some(paint?),
            "border" => spec.border = Some((paint?, spec.border.map_or(1.0, |b| b.1))),
            "radius" => spec.radius = Some(radius?),
            _ => return None,
        }
        Some(())
    }

    fn to_paint(&self, v: &TokenValue) -> Option<Paint> {
        Some(match v {
            TokenValue::Color(c) => Paint::Literal(*c),
            TokenValue::Ref(r) => Paint::Role(ColorRole::parse(r)?),
            TokenValue::Alpha { base, alpha } => match (ColorRole::parse(base)?, alpha.parse::<f32>()) {
                (r, Ok(a)) => Paint::Scaled(r, a),
                (r, Err(_)) => Paint::Alpha(r, AlphaRole::parse(alpha)?),
            },
            TokenValue::Number(_) => return None,
        })
    }

    /// Applies a config `[theme]` table; nested tables join into dotted keys. Literal values go
    /// first so references see them. Returns what failed.
    pub fn apply_table(&mut self, t: &toml::Table) -> Vec<ThemeError> {
        let mut flat = Vec::new();
        flatten("", t, &mut flat);
        let mut errors = Vec::new();
        let mut values: Vec<_> = flat
            .into_iter()
            .filter_map(|(k, v)| match v {
                toml::Value::String(s) => Some((k, TokenValue::parse(s))),
                toml::Value::Integer(i) => Some((k, TokenValue::Number(*i as f32))),
                toml::Value::Float(f) => Some((k, TokenValue::Number(*f as f32))),
                other => {
                    errors.push(ThemeError::BadValue(k, other.to_string()));
                    None
                }
            })
            .collect();
        values.sort_by_key(|(_, v)| matches!(v, TokenValue::Ref(_) | TokenValue::Alpha { .. }));
        errors.extend(values.into_iter().filter_map(|(k, v)| self.apply(&k, v).err()));
        errors
    }
}

fn flatten<'a>(prefix: &str, t: &'a toml::Table, out: &mut Vec<(String, &'a toml::Value)>) {
    for (k, v) in t {
        let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match v {
            toml::Value::Table(inner) => flatten(&key, inner, out),
            v => out.push((key, v)),
        }
    }
}

impl TokenValue {
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if let Some(c) = s.strip_prefix('#').and_then(hex) { return Self::Color(c) }
        if let Ok(n) = s.parse() { return Self::Number(n) }
        match s.split_once('@') {
            Some((base, alpha)) => Self::Alpha { base: base.trim().to_owned(), alpha: alpha.trim().to_owned() },
            None => Self::Ref(s.to_owned()),
        }
    }
}

fn hex(s: &str) -> Option<Color> {
    let v = u32::from_str_radix(s, 16).ok()?;
    let b = |shift: u32| ((v >> shift) & 0xff) as u8;
    match s.len() {
        6 => Some(Color::from_rgb8(b(16), b(8), b(0))),
        8 => Some(Color::from_rgba8(b(24), b(16), b(8), f32::from(b(0)) / 255.0)),
        _ => None,
    }
}
