//! Terminal colours from the theme tokens: the 16 ANSI colours are the `term.N` roles, the rest
//! come from the ground, text, cursor and accent roles; the 256-colour cube and gray ramp are
//! xterm's.

use alacritty_terminal::{term::color::Colors, vte::ansi::{Color as AnsiColor, NamedColor, Rgb}};
use iced::{theme::palette::mix, Color, Theme, color};
use loa_ui_kit::theme::{AlphaRole, ColorRole as C, for_theme};

/// `ansi` holds the normal (0..8) and bright (8..16) ANSI colours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TermPalette { pub ansi: [Color; 16], pub foreground: Color, pub background: Color, pub cursor: Color, pub selection: Color }

impl TermPalette {
    /// The colours of the tokens behind `theme`; a new set after a theme change redraws.
    pub fn from_theme(theme: &Theme) -> Self {
        let t = for_theme(theme);
        let selection = t.color(C::Accent).scale_alpha(t.alpha(AlphaRole::Tint));
        Self { ansi: std::array::from_fn(|n| t.color(C::term(n))), foreground: t.color(C::Text), background: t.color(C::Ground), cursor: t.color(C::CursorRing), selection }
    }
    /// A cell colour; `overrides` set by the program (OSC 4/10/11) win over the theme.
    pub fn resolve(&self, color: AnsiColor, overrides: &Colors) -> Color {
        let rgb = |c: Rgb| color!(c.r, c.g, c.b);
        match color {
            AnsiColor::Spec(c) => rgb(c),
            AnsiColor::Indexed(i) => overrides[usize::from(i)].map_or_else(|| self.indexed(i), rgb),
            AnsiColor::Named(n) => overrides[n].map_or_else(|| self.named(n), rgb),
        }
    }
    fn named(&self, name: NamedColor) -> Color {
        match name {
            n if (n as usize) < 16 => self.ansi[n as usize],
            NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            NamedColor::DimForeground => mix(self.foreground, self.background, 0.33),
            // DimBlack..=DimWhite follow Cursor in declaration order.
            dim => mix(self.ansi[(dim as usize).saturating_sub(NamedColor::DimBlack as usize) % 8], self.background, 0.33),
        }
    }
    fn indexed(&self, i: u8) -> Color {
        let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
        match i {
            0..16 => self.ansi[usize::from(i)],
            16..232 => color!(level((i - 16) / 36), level((i - 16) / 6 % 6), level((i - 16) % 6)),
            232.. => color!(8 + (i - 232) * 10, 8 + (i - 232) * 10, 8 + (i - 232) * 10),
        }
    }
}

#[cfg(test)]
#[test]
fn colours_resolve_from_tokens_overrides_and_xterm_tables() {
    let (mut set, none, t) = (Colors::default(), Colors::default(), for_theme(&Theme::Light));
    set[NamedColor::Background] = Some(Rgb { r: 1, g: 2, b: 3 });
    let cases = [
        (AnsiColor::Named(NamedColor::Background), none, t.color(C::Ground)),
        (AnsiColor::Named(NamedColor::Red), none, t.color(C::Term1)),
        (AnsiColor::Named(NamedColor::Background), set, color!(1, 2, 3)),
        (AnsiColor::Indexed(196), none, color!(255, 0, 0)),
        (AnsiColor::Indexed(244), none, color!(128, 128, 128)),
    ];
    for (c, o, want) in cases { assert_eq!(TermPalette::from_theme(&Theme::Light).resolve(c, &o), want, "{c:?}") }
}
