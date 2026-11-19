//! Terminal colors derived from the app theme.
//!
//! The 16 ANSI colors, foreground, background, cursor and selection all come
//! from the iced extended palette, so the dark and light themes apply to the
//! terminal too. The 256-color cube and gray ramp are the xterm definitions.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb};
use iced::theme::palette::mix;
use iced::{Color, Theme};

/// Every color a terminal cell can resolve to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TermPalette {
    /// Normal (0..8) and bright (8..16) ANSI colors.
    pub ansi: [Color; 16],
    /// Default text color.
    pub foreground: Color,
    /// Default background.
    pub background: Color,
    /// Block cursor color.
    pub cursor: Color,
    /// Background of selected cells.
    pub selection: Color,
    /// Text on selected cells.
    pub selection_text: Color,
}

impl TermPalette {
    /// Derives the terminal colors from `theme`.
    pub fn from_theme(theme: &Theme) -> Self {
        let palette = theme.extended_palette();
        let background = palette.background.base.color;
        let foreground = palette.background.base.text;
        // ANSI black is the dark end and white the light end, whichever of
        // background and text is darker, so both theme kinds read correctly.
        let (dark, light) = if palette.is_dark {
            (background, foreground)
        } else {
            (foreground, background)
        };
        let red = palette.danger.base.color;
        let green = palette.success.base.color;
        let yellow = palette.warning.base.color;
        // The theme's primary is a neutral grey, so blue is the success green
        // turned 80° around the hue circle: as saturated and bright as the
        // other ANSI hues of the theme.
        let blue = rotate_hue(green, 80.0);
        let magenta = mix(red, blue, 0.5);
        let cyan = mix(green, blue, 0.5);
        let normal = [
            mix(dark, light, 0.1),
            red,
            green,
            yellow,
            blue,
            magenta,
            cyan,
            mix(dark, light, 0.75),
        ];
        let brighten = |color: Color| mix(color, light, 0.3);
        let ansi = [
            normal[0],
            normal[1],
            normal[2],
            normal[3],
            normal[4],
            normal[5],
            normal[6],
            normal[7],
            mix(dark, light, 0.45),
            brighten(red),
            brighten(green),
            brighten(yellow),
            brighten(blue),
            brighten(magenta),
            brighten(cyan),
            light,
        ];
        Self {
            ansi,
            foreground,
            background,
            cursor: palette.primary.strong.color,
            selection: palette.primary.weak.color,
            selection_text: palette.primary.weak.text,
        }
    }

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
                Color::from_rgb8(level(cube / 36), level((cube / 6) % 6), level(cube % 6))
            }
            232.. => {
                let gray = 8 + (index - 232) * 10;
                Color::from_rgb8(gray, gray, gray)
            }
        }
    }
}

fn rgb_to_color(rgb: Rgb) -> Color {
    Color::from_rgb8(rgb.r, rgb.g, rgb.b)
}

/// `color` with its hue turned by `degrees`, keeping saturation, lightness
/// and alpha.
fn rotate_hue(color: Color, degrees: f32) -> Color {
    let max = color.r.max(color.g).max(color.b);
    let min = color.r.min(color.g).min(color.b);
    let lightness = (max + min) / 2.0;
    let delta = max - min;
    if delta <= f32::EPSILON {
        return color;
    }
    let hue = if max == color.r {
        60.0 * ((color.g - color.b) / delta).rem_euclid(6.0)
    } else if max == color.g {
        60.0 * ((color.b - color.r) / delta + 2.0)
    } else {
        60.0 * ((color.r - color.g) / delta + 4.0)
    };
    let hue = (hue + degrees).rem_euclid(360.0);
    let chroma = delta;
    let x = chroma * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hue {
        h if h < 60.0 => (chroma, x, 0.0),
        h if h < 120.0 => (x, chroma, 0.0),
        h if h < 180.0 => (0.0, chroma, x),
        h if h < 240.0 => (0.0, x, chroma),
        h if h < 300.0 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = lightness - chroma / 2.0;
    Color { r: r + m, g: g + m, b: b + m, a: color.a }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_colors_follow_the_theme() {
        let overrides = Colors::default();
        for theme in [Theme::Dark, Theme::Light, Theme::Dracula] {
            let palette = TermPalette::from_theme(&theme);
            let ext = theme.extended_palette();
            let bg = palette.resolve(AnsiColor::Named(NamedColor::Background), &overrides);
            let fg = palette.resolve(AnsiColor::Named(NamedColor::Foreground), &overrides);
            let red = palette.resolve(AnsiColor::Named(NamedColor::Red), &overrides);
            assert_eq!(bg, ext.background.base.color);
            assert_eq!(fg, ext.background.base.text);
            assert_eq!(red, ext.danger.base.color);
        }
        assert_ne!(
            TermPalette::from_theme(&Theme::Dark).background,
            TermPalette::from_theme(&Theme::Light).background
        );
    }

    #[test]
    fn black_and_white_stay_readable_in_light_and_dark_themes() {
        for theme in [Theme::Dark, Theme::Light] {
            let palette = TermPalette::from_theme(&theme);
            let luma = |c: Color| 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
            assert!(luma(palette.ansi[0]) < luma(palette.ansi[7]), "{theme:?}");
            assert!(luma(palette.ansi[8]) < luma(palette.ansi[15]), "{theme:?}");
        }
    }

    #[test]
    fn program_overrides_and_truecolor_win_over_the_theme() {
        let palette = TermPalette::from_theme(&Theme::Dark);
        let mut overrides = Colors::default();
        overrides[NamedColor::Background] = Some(Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(
            palette.resolve(AnsiColor::Named(NamedColor::Background), &overrides),
            Color::from_rgb8(1, 2, 3)
        );
        assert_eq!(
            palette.resolve(AnsiColor::Spec(Rgb { r: 9, g: 8, b: 7 }), &Colors::default()),
            Color::from_rgb8(9, 8, 7)
        );
    }

    #[test]
    fn xterm_cube_and_gray_ramp() {
        let palette = TermPalette::from_theme(&Theme::Dark);
        let none = Colors::default();
        assert_eq!(palette.resolve(AnsiColor::Indexed(196), &none), Color::from_rgb8(255, 0, 0));
        assert_eq!(palette.resolve(AnsiColor::Indexed(16), &none), Color::from_rgb8(0, 0, 0));
        assert_eq!(palette.resolve(AnsiColor::Indexed(244), &none), Color::from_rgb8(128, 128, 128));
        assert_eq!(palette.resolve(AnsiColor::Indexed(1), &none), palette.ansi[1]);
    }

    #[test]
    fn ansi_blue_stays_blue_under_a_neutral_primary() {
        let mut colors = Theme::Dark.palette();
        colors.primary = mix(colors.background, colors.text, 0.8);
        let palette = TermPalette::from_theme(&Theme::custom("Neutral".to_owned(), colors));
        let blue = palette.ansi[4];
        assert!(blue.b > blue.r && blue.b > blue.g, "{blue:?}");
    }
}
