//! Software drawing used by the simple page renderers (XLSX grid, DOCX page, PPTX slide): an RGBA canvas,
//! rectangles, scaled image blits and text through a system font found at runtime. Without a font, text
//! degrades to one outlined box per character so the layout stays visible.

use crate::docs::Rgba;
use ab_glyph::{Font, FontArc, GlyphId, PxScale, PxScaleFont, ScaleFont, point};
use std::path::{Path, PathBuf};

/// Straight (not premultiplied) RGBA color.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Color(pub [u8; 4]);

/// Colours of rendered pages.
#[derive(Clone, Debug, PartialEq)]
pub struct DocPalette {
    pub page: Color,
    pub text: Color,
    /// Table borders, header rules and picture placeholders.
    pub rule: Color,
    /// Sheet grid lines and slide table outlines.
    pub grid: Color,
    pub header: Color,
    pub header_text: Color,
}

impl Default for DocPalette {
    fn default() -> Self {
        let grey = |v| Color::rgb(v, v, v);
        Self { page: Color::WHITE, text: Color::BLACK, rule: grey(128), grid: grey(217), header: grey(242), header_text: grey(64) }
    }
}

/// Default font sizes in points.
#[derive(Clone, Debug, PartialEq)]
pub struct DocTypography {
    /// DOCX body text (laid out at parse time with the default).
    pub body: f32,
    pub cell: f32,
    pub slide: f32,
}

impl Default for DocTypography {
    fn default() -> Self {
        Self { body: 11.0, cell: 11.0, slide: 18.0 }
    }
}

/// How pages are drawn; the UI fills it from its theme tokens.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderOpts {
    pub palette: DocPalette,
    pub typography: DocTypography,
}

impl RenderOpts {
    /// Body text of `size` canvas pixels in the palette's text colour.
    pub fn text(&self, size: f32) -> TextStyle {
        TextStyle::new(size).color(self.palette.text)
    }
}

impl Color {
    pub const WHITE: Self = Self([255, 255, 255, 255]);
    pub const BLACK: Self = Self([0, 0, 0, 255]);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b, 255])
    }
}

/// How a run of text is drawn.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// Font size in canvas pixels.
    pub size: f32,
    pub bold: bool,
    pub color: Color,
}

impl TextStyle {
    pub const fn new(size: f32) -> Self {
        Self { size, bold: false, color: Color::BLACK }
    }

    pub const fn bold(mut self, bold: bool) -> Self {
        self.bold = bold;
        self
    }

    pub const fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }
}

/// Font files known by name, most preferred first.
const REGULAR_NAMES: &[&str] = &[
    "DejaVuSans.ttf", "LiberationSans-Regular.ttf", "NotoSans-Regular.ttf", "FreeSans.ttf", "Ubuntu-R.ttf",
    "Arial.ttf", "arial.ttf", "SFNS.ttf", "segoeui.ttf",
];
const BOLD_NAMES: &[&str] = &[
    "DejaVuSans-Bold.ttf", "LiberationSans-Bold.ttf", "NotoSans-Bold.ttf", "FreeSansBold.ttf", "Ubuntu-B.ttf",
    "Arial Bold.ttf", "arialbd.ttf", "segoeuib.ttf",
];
/// System wide font directories on Linux, macOS and Windows.
const SYSTEM_FONT_DIRS: &[&str] =
    &["/usr/share/fonts", "/usr/local/share/fonts", "/System/Library/Fonts", "/Library/Fonts", "C:\\Windows\\Fonts"];
/// How deep font directories are searched.
const MAX_DEPTH: usize = 4;

/// The fonts text is drawn with. Cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct Fonts {
    regular: Option<FontArc>,
    bold: Option<FontArc>,
}

impl Fonts {
    /// No font: text is drawn as boxes.
    pub fn none() -> Self {
        Self::default()
    }

    /// Searches the system font directories for a sans serif font: known families first, else any `.ttf`/`.otf`.
    pub fn system() -> Self {
        let mut files = Vec::new();
        for dir in SYSTEM_FONT_DIRS {
            collect_font_files(Path::new(dir), MAX_DEPTH, &mut files);
        }
        files.sort();
        let load = |path: &PathBuf| FontArc::try_from_vec(std::fs::read(path).ok()?).ok();
        let find = |names: &[&str]| {
            names.iter().find_map(|name| files.iter().filter(|f| f.file_name().is_some_and(|n| n == *name)).find_map(load))
        };
        Self { regular: find(REGULAR_NAMES).or_else(|| files.iter().find_map(load)), bold: find(BOLD_NAMES) }
    }

    fn pick(&self, bold: bool) -> Option<&FontArc> {
        if bold { self.bold.as_ref().or(self.regular.as_ref()) } else { self.regular.as_ref() }
    }

    /// Distance between two baselines.
    pub fn line_height(&self, size: f32) -> f32 {
        size * 1.25
    }

    /// Horizontal advance of `text` drawn at `size` pixels.
    pub fn text_width(&self, text: &str, size: f32, bold: bool) -> f32 {
        match self.pick(bold) {
            Some(font) => glyphs(&font.as_scaled(PxScale::from(size)), text, |_, _, _, _| {}),
            None => text.chars().count() as f32 * box_advance(size),
        }
    }

    /// Word wraps `text` to `max_width`; newlines always break, a word wider than the line gets its own line.
    pub fn wrap<'t>(&self, text: &'t str, size: f32, bold: bool, max_width: f32) -> Vec<&'t str> {
        let mut lines = Vec::new();
        for paragraph in text.split('\n') {
            let mut start = 0;
            let mut end = 0;
            for (index, word) in word_starts(paragraph) {
                let candidate = &paragraph[start..index + word.len()];
                if end > start && self.text_width(candidate.trim_end(), size, bold) > max_width {
                    lines.push(paragraph[start..end].trim_end());
                    start = index + word.len() - word.trim_start_matches(' ').len();
                }
                end = index + word.len();
            }
            lines.push(paragraph[start..].trim_end());
        }
        lines
    }
}

/// Lays `text` out with kerning, calling `f(char, glyph, caret, advance)` per character; returns the advance.
fn glyphs(scaled: &PxScaleFont<&FontArc>, text: &str, mut f: impl FnMut(char, GlyphId, f32, f32)) -> f32 {
    let (mut caret, mut last) = (0.0, None);
    for c in text.chars() {
        let id = scaled.glyph_id(c);
        if let Some(prev) = last {
            caret += scaled.kern(prev, id);
        }
        let advance = scaled.h_advance(id);
        f(c, id, caret, advance);
        caret += advance;
        last = Some(id);
    }
    caret
}

/// Words with their byte offsets, each with its leading spaces.
fn word_starts(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    std::iter::from_fn(move || {
        let (start, rest) = (offset, &text[offset..]);
        let lead = rest.len() - rest.trim_start_matches(' ').len();
        offset += lead + rest[lead..].find(' ').unwrap_or(rest.len() - lead);
        (offset > start).then(|| (start, &rest[..offset - start]))
    })
}

fn box_advance(size: f32) -> f32 {
    size * 0.6
}

fn collect_font_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if depth > 0 {
                collect_font_files(&path, depth - 1, out);
            }
        } else if (path.extension().and_then(|e| e.to_str()))
            .is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"))
        {
            out.push(path);
        }
    }
}

/// An RGBA drawing surface in pixels.
#[derive(Clone, Debug)]
pub struct Canvas(Rgba);

impl Canvas {
    pub fn new(width: u32, height: u32, background: Color) -> Self {
        Self(Rgba::filled(width, height, background.0))
    }

    pub fn height(&self) -> u32 {
        self.0.height
    }

    /// Blends `color` at `coverage` (0 to 1) over the pixel at `(x, y)`.
    fn blend(&mut self, x: i64, y: i64, color: Color, coverage: f32) {
        if x < 0 || y < 0 || x >= i64::from(self.0.width) || y >= i64::from(self.0.height) {
            return;
        }
        let alpha = coverage.clamp(0.0, 1.0) * f32::from(color.0[3]) / 255.0;
        if alpha <= 0.0 {
            return;
        }
        let i = (y as usize * self.0.width as usize + x as usize) * 4;
        let px = &mut self.0.data[i..i + 4];
        for (p, &src) in px[..3].iter_mut().zip(&color.0[..3]) {
            let dst = f32::from(*p);
            *p = (dst + (f32::from(src) - dst) * alpha).round() as u8;
        }
        let dst_a = f32::from(px[3]) / 255.0;
        px[3] = ((alpha + dst_a * (1.0 - alpha)) * 255.0).round() as u8;
    }

    /// Clamps a float span to pixel indexes inside `0..limit`.
    fn span(start: f32, len: f32, limit: u32) -> (i64, i64) {
        (start.round().max(0.0) as i64, ((start + len).round() as i64).min(i64::from(limit)))
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color) {
        let (x0, x1) = Self::span(x, w, self.0.width);
        let (y0, y1) = Self::span(y, h, self.0.height);
        for py in y0..y1 {
            for px in x0..x1 {
                self.blend(px, py, color, 1.0);
            }
        }
    }

    /// Outline of a rectangle, `thickness` pixels wide, drawn inside it.
    pub fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32, thickness: f32, color: Color) {
        let t = thickness.max(1.0);
        self.fill_rect(x, y, w, t, color);
        self.fill_rect(x, y + h - t, w, t, color);
        self.fill_rect(x, y, t, h, color);
        self.fill_rect(x + w - t, y, t, h, color);
    }

    /// Draws `image` scaled (nearest neighbour) into the given rectangle.
    pub fn blit(&mut self, image: &Rgba, x: f32, y: f32, w: f32, h: f32) {
        if image.width == 0 || image.height == 0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let (x0, x1) = Self::span(x, w, self.0.width);
        let (y0, y1) = Self::span(y, h, self.0.height);
        for py in y0..y1 {
            let sy = (((py as f32 + 0.5 - y) / h * image.height as f32) as u32).min(image.height - 1);
            for px in x0..x1 {
                let sx = (((px as f32 + 0.5 - x) / w * image.width as f32) as u32).min(image.width - 1);
                let i = (sy as usize * image.width as usize + sx as usize) * 4;
                let s = &image.data[i..i + 4];
                self.blend(px, py, Color([s[0], s[1], s[2], 255]), f32::from(s[3]) / 255.0);
            }
        }
    }

    /// Draws one line of text with its top edge at `y`. Returns the advance.
    pub fn draw_text(&mut self, fonts: &Fonts, style: TextStyle, x: f32, y: f32, text: &str) -> f32 {
        let (size, color) = (style.size, style.color);
        let Some(font) = fonts.pick(style.bold) else {
            // One box per character.
            let (advance, h) = (box_advance(size), size * 0.7);
            for (i, c) in text.chars().enumerate() {
                if !c.is_whitespace() {
                    self.stroke_rect(x + i as f32 * advance + 1.0, y + size * 0.25, (advance - 2.0).max(1.0), h, 1.0, color);
                }
            }
            return text.chars().count() as f32 * advance;
        };
        let scaled = font.as_scaled(PxScale::from(size));
        let baseline = y + scaled.ascent();
        glyphs(&scaled, text, |c, id, caret, advance| {
            let caret = x + caret;
            if id.0 == 0 && !c.is_whitespace() {
                let h = size * 0.7;
                self.stroke_rect(caret + 1.0, baseline - h, (advance - 2.0).max(1.0), h, 1.0, color);
            } else if let Some(outline) = font.outline_glyph(id.with_scale_and_position(size, point(caret, baseline))) {
                let min = outline.px_bounds().min;
                outline.draw(|gx, gy, coverage| {
                    self.blend(min.x as i64 + i64::from(gx), min.y as i64 + i64::from(gy), color, coverage);
                });
            }
        })
    }

    pub fn into_rgba(self) -> Rgba {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_font_text_is_boxes_and_wraps_between_words() {
        let mut canvas = Canvas::new(200, 40, Color::WHITE);
        let advance = canvas.draw_text(&Fonts::none(), TextStyle::new(20.0), 0.0, 0.0, "ab c");
        assert_eq!(advance, 4.0 * box_advance(20.0));
        let image = canvas.into_rgba();
        let column = |x: u32| (0..40).any(|y| image.pixel(x, y).is_some_and(|p| p[0] < 128));
        assert!(column((box_advance(20.0) * 0.5) as u32) && !column((box_advance(20.0) * 2.5) as u32));
        let w = box_advance(10.0);
        for (text, width, lines) in [("aaa bbb ccc\nddd", 7.5, &["aaa bbb", "ccc", "ddd"][..]), ("abcdefghij", 3.0, &["abcdefghij"])] {
            assert_eq!(Fonts::none().wrap(text, 10.0, false, w * width), lines);
        }
    }
}
