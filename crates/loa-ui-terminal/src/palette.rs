//! Terminal colors from the theme tokens: the 16 ANSI colours are the `term.N` roles, the
//! rest come from the ground, text and accent roles. The 256-color cube and gray ramp are the
//! xterm definitions.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb};
use iced::theme::palette::mix;
use iced::{Color, Theme, color};
use loa_ui_kit::theme::{AlphaRole, ColorRole as C, TokenSet, for_theme};

/// Every color a terminal cell can resolve to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TermPalette {
    /// Normal (0..8) and bright (8..16) ANSI colors.
    pub ansi: [Color; 16],
    pub foreground: Color,
    pub background: Color,
    pub cursor: Color,
    /// Background of selected cells.
    pub selection: Color,
    /// Text on selected cells.
    pub selection_text: Color,
}

impl TermPalette {
    /// The terminal colors of `t`.
    pub fn from_tokens(t: &TokenSet) -> Self {
        let foreground = t.color(C::Text);
        Self {
            ansi: std::array::from_fn(|n| t.color(C::term(n))),
            foreground,
            background: t.color(C::Ground),
            cursor: t.color(C::CursorRing),
            selection: t.color(C::Accent).scale_alpha(t.alpha(AlphaRole::Tint)),
            selection_text: foreground,
        }
    }

    /// The terminal colors of the tokens behind `theme`; a new set after a theme change makes
    /// the canvas redraw.
    pub fn from_theme(theme: &Theme) -> Self { Self::from_tokens(&for_theme(theme)) }

    /// Resolves a cell color. `overrides` are colors the program set with
    /// OSC 4/10/11 and win over the theme.
    pub fn resolve(&self, color: AnsiColor, overrides: &Colors) -> Color {
        match color {
            AnsiColor::Spec(rgb) => rgb_to_color(rgb),
            AnsiColor::Indexed(index) => match overrides[usize::from(index)] {
                Some(rgb) => rgb_to_color(rgb),
                None => self.indexed(index),
            },
            AnsiColor::Named(name) => match overrides[name] {
                Some(rgb) => rgb_to_color(rgb),
                None => self.named(name),
            },
        }
    }

    fn named(&self, name: NamedColor) -> Color {
        let index = name as usize;
        if index < 16 {
            return self.ansi[index];
        }
        match name {
            NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            NamedColor::DimForeground => mix(self.foreground, self.background, 0.33),
            dim => {
                // DimBlack..=DimWhite follow Cursor in declaration order.
                let base = (dim as usize).saturating_sub(NamedColor::DimBlack as usize) % 8;
                mix(self.ansi[base], self.background, 0.33)
            }
        }
    }

    fn indexed(&self, index: u8) -> Color {
        match index {
            0..16 => self.ansi[usize::from(index)],
            16..232 => {
                let cube = index - 16;
                let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
                color!(level(cube / 36), level((cube / 6) % 6), level(cube % 6))
            }
            232.. => {
                let gray = 8 + (index - 232) * 10;
                color!(gray, gray, gray)
            }
        }
    }
}

fn rgb_to_color(rgb: Rgb) -> Color { color!(rgb.r, rgb.g, rgb.b) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_colors_follow_the_tokens() {
        let overrides = Colors::default();
        for theme in [Theme::Dark, Theme::Light] {
            let (palette, t) = (TermPalette::from_theme(&theme), for_theme(&theme));
            let named = |n| palette.resolve(AnsiColor::Named(n), &overrides);
            assert_eq!((named(NamedColor::Background), named(NamedColor::Foreground)), (t.color(C::Ground), t.color(C::Text)));
            assert_eq!(named(NamedColor::Red), t.color(C::Term1));
            let luma = |c: Color| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
            assert!(luma(palette.ansi[4]) > 0.0 && palette.ansi[4].b > palette.ansi[4].r, "{theme:?}");
        }
        assert_ne!(TermPalette::from_theme(&Theme::Dark).background, TermPalette::from_theme(&Theme::Light).background);
    }

    #[test]
    fn program_overrides_and_truecolor_win_over_the_theme() {
        let palette = TermPalette::from_theme(&Theme::Dark);
        let mut overrides = Colors::default();
        overrides[NamedColor::Background] = Some(Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(palette.resolve(AnsiColor::Named(NamedColor::Background), &overrides), color!(1, 2, 3));
        assert_eq!(palette.resolve(AnsiColor::Spec(Rgb { r: 9, g: 8, b: 7 }), &Colors::default()), color!(9, 8, 7));
    }

    #[test]
    fn xterm_cube_and_gray_ramp() {
        let palette = TermPalette::from_theme(&Theme::Dark);
        let none = Colors::default();
        assert_eq!(palette.resolve(AnsiColor::Indexed(196), &none), color!(255, 0, 0));
        assert_eq!(palette.resolve(AnsiColor::Indexed(16), &none), color!(0, 0, 0));
        assert_eq!(palette.resolve(AnsiColor::Indexed(244), &none), color!(128, 128, 128));
        assert_eq!(palette.resolve(AnsiColor::Indexed(1), &none), palette.ansi[1]);
    }
}
