//! Terminal colours from the theme tokens: the 16 ANSI colours are the `term.N` roles, the rest
//! come from the ground, text, cursor and accent roles; the 256-colour cube and gray ramp are
//! xterm's.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb};
use iced::theme::palette::mix;
use iced::{Color, Theme, color};
use loa_ui_kit::theme::{AlphaRole, ColorRole as C, TokenSet, for_theme};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TermPalette {
    /// Normal (0..8) and bright (8..16) ANSI colours.
    pub ansi: [Color; 16],
    pub foreground: Color,
    pub background: Color,
    pub cursor: Color,
    pub selection: Color,
    pub selection_text: Color,
}

impl TermPalette {
    pub fn from_tokens(t: &TokenSet) -> Self {
        let foreground = t.color(C::Text);
        let selection = t.color(C::Accent).scale_alpha(t.alpha(AlphaRole::Tint));
        Self { ansi: std::array::from_fn(|n| t.color(C::term(n))), foreground, background: t.color(C::Ground), cursor: t.color(C::CursorRing), selection, selection_text: foreground }
    }

    /// The colours of the tokens behind `theme`; a new set after a theme change redraws.
    pub fn from_theme(theme: &Theme) -> Self { Self::from_tokens(&for_theme(theme)) }

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
mod tests {
    use super::*;

    #[test]
    fn colours_resolve_from_tokens_overrides_and_xterm_tables() {
        let mut overrides = Colors::default();
        overrides[NamedColor::Background] = Some(Rgb { r: 1, g: 2, b: 3 });
        for theme in [Theme::Dark, Theme::Light] {
            let (p, t) = (TermPalette::from_theme(&theme), for_theme(&theme));
            let cases = [
                (AnsiColor::Named(NamedColor::Background), Colors::default(), t.color(C::Ground)),
                (AnsiColor::Named(NamedColor::Foreground), Colors::default(), t.color(C::Text)),
                (AnsiColor::Named(NamedColor::Red), Colors::default(), t.color(C::Term1)),
                (AnsiColor::Indexed(1), Colors::default(), t.color(C::Term1)),
                (AnsiColor::Named(NamedColor::Background), overrides, color!(1, 2, 3)),
                (AnsiColor::Spec(Rgb { r: 9, g: 8, b: 7 }), Colors::default(), color!(9, 8, 7)),
                (AnsiColor::Indexed(196), Colors::default(), color!(255, 0, 0)),
                (AnsiColor::Indexed(16), Colors::default(), color!(0, 0, 0)),
                (AnsiColor::Indexed(244), Colors::default(), color!(128, 128, 128)),
            ];
            for (c, o, want) in cases { assert_eq!(p.resolve(c, &o), want, "{theme:?} {c:?}") }
        }
        assert_ne!(TermPalette::from_theme(&Theme::Dark).background, TermPalette::from_theme(&Theme::Light).background);
    }
}
