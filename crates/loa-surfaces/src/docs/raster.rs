//! Software drawing used by the simple page renderers (XLSX grid, DOCX page,
//! PPTX slide) and by the screenshot path: an RGBA canvas, rectangles,
//! scaled image blits and text through a system font found at runtime.
//!
//! When no font is found, text degrades to one outlined box per character so
//! the layout stays visible.

use crate::docs::Rgba;
use ab_glyph::{Font, FontArc, PxScale, ScaleFont, point};
use std::path::{Path, PathBuf};

/// Straight (not premultiplied) RGBA color.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Color(pub [u8; 4]);

impl Color {
    pub const WHITE: Self = Self([255, 255, 255, 255]);
    pub const BLACK: Self = Self([0, 0, 0, 255]);
    pub const GRAY: Self = Self([128, 128, 128, 255]);
    pub const LIGHT_GRAY: Self = Self([217, 217, 217, 255]);
    pub const HEADER: Self = Self([242, 242, 242, 255]);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b, 255])
    }

    /// Parses `RRGGBB` (as found in OOXML `val` attributes), with or without `#`.
    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.strip_prefix('#').unwrap_or(hex);
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Self::rgb(channel(0)?, channel(2)?, channel(4)?))
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
    "DejaVuSans.ttf",
    "LiberationSans-Regular.ttf",
    "NotoSans-Regular.ttf",
    "FreeSans.ttf",
    "Ubuntu-R.ttf",
    "Arial.ttf",
    "arial.ttf",
    "Helvetica.ttc",
    "SFNS.ttf",
    "segoeui.ttf",
];

const BOLD_NAMES: &[&str] = &[
    "DejaVuSans-Bold.ttf",
    "LiberationSans-Bold.ttf",
    "NotoSans-Bold.ttf",
    "FreeSansBold.ttf",
    "Ubuntu-B.ttf",
    "Arial Bold.ttf",
    "arialbd.ttf",
    "segoeuib.ttf",
];

/// System wide font directories on Linux, macOS and Windows.
const SYSTEM_FONT_DIRS: &[&str] = &[
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    "/System/Library/Fonts",
    "/Library/Fonts",
    "C:\\Windows\\Fonts",
];

/// How deep font directories are searched.
const MAX_DEPTH: usize = 4;

/// The fonts text is drawn with. Cheap to clone.
#[derive(Clone, Default)]
pub struct Fonts {
    regular: Option<FontArc>,
    bold: Option<FontArc>,
}

impl std::fmt::Debug for Fonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fonts")
            .field("regular", &self.regular.is_some())
            .field("bold", &self.bold.is_some())
            .finish()
    }
}

impl Fonts {
    /// No font: text is drawn as boxes.
    pub fn none() -> Self {
        Self::default()
    }

    /// Searches the operating system's shared font directories. This is the
    /// only place the crate looks at global state for fonts.
    pub fn system() -> Self {
        Self::from_dirs(SYSTEM_FONT_DIRS)
    }

    /// Searches `dirs` (recursively, a few levels deep) for a sans serif font.
    /// Known families win; otherwise the first `.ttf`/`.otf` found is used.
    pub fn from_dirs<P: AsRef<Path>>(dirs: &[P]) -> Self {
        let mut files = Vec::new();
        for dir in dirs {
            collect_font_files(dir.as_ref(), MAX_DEPTH, &mut files);
        }
        files.sort();
        let find = |names: &[&str]| {
            names.iter().find_map(|name| {
                files
                    .iter()
                    .filter(|f| f.file_name().is_some_and(|n| n == *name))
                    .find_map(|f| load_font(f))
            })
        };
        let regular = find(REGULAR_NAMES).or_else(|| files.iter().find_map(|f| load_font(f)));
        let bold = find(BOLD_NAMES);
        Self { regular, bold }
    }

    /// Fonts from file contents (TTF or OTF).
    pub fn from_bytes(regular: Vec<u8>, bold: Option<Vec<u8>>) -> crate::docs::Result<Self> {
        let parse = |data: Vec<u8>| {
            FontArc::try_from_vec(data).map_err(|e| crate::docs::DocsError::Font(e.to_string()))
        };
        Ok(Self { regular: Some(parse(regular)?), bold: bold.map(parse).transpose()? })
    }

    /// Whether real glyphs are drawn (as opposed to boxes).
    pub fn has_font(&self) -> bool {
        self.regular.is_some()
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
            Some(font) => {
                let scaled = font.as_scaled(PxScale::from(size));
                let mut width = 0.0;
                let mut last = None;
                for c in text.chars() {
                    let id = scaled.glyph_id(c);
                    if let Some(prev) = last {
                        width += scaled.kern(prev, id);
                    }
                    width += scaled.h_advance(id);
                    last = Some(id);
                }
                width
            }
            None => text.chars().count() as f32 * box_advance(size),
        }
    }

    /// Word wraps `text` to `max_width`. Explicit newlines always break. A
    /// word wider than the line is put on its own line.
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

/// Words with their byte offsets, keeping each word's leading spaces with the
/// previous one.
fn word_starts(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut rest = text;
    let mut offset = 0;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let lead = rest.len() - rest.trim_start_matches(' ').len();
        let body = &rest[lead..];
        let len = body.find(' ').unwrap_or(body.len());
        let start = offset;
        let word = &rest[..lead + len];
        rest = &rest[lead + len..];
        offset += lead + len;
        Some((start, word))
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
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"))
        {
            out.push(path);
        }
    }
}

fn load_font(path: &Path) -> Option<FontArc> {
    FontArc::try_from_vec(std::fs::read(path).ok()?).ok()
}

/// An RGBA drawing surface in pixels.
#[derive(Clone, Debug)]
pub struct Canvas(Rgba);

impl Canvas {
    pub fn new(width: u32, height: u32, background: Color) -> Self {
        Self(Rgba::filled(width, height, background.0))
    }

    pub fn width(&self) -> u32 {
        self.0.width
    }

    pub fn height(&self) -> u32 {
        self.0.height
    }

    /// The pixel at `(x, y)`, for tests and hit checks.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        self.0.pixel(x, y)
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
        let a = start.round().max(0.0) as i64;
        let b = ((start + len).round() as i64).min(i64::from(limit));
        (a, b)
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
        let Some(font) = fonts.pick(style.bold) else {
            return self.draw_boxes(style, x, y, text);
        };
        let scaled = font.as_scaled(PxScale::from(style.size));
        let baseline = y + scaled.ascent();
        let mut caret = x;
        let mut last = None;
        for c in text.chars() {
            let id = scaled.glyph_id(c);
            if let Some(prev) = last {
                caret += scaled.kern(prev, id);
            }
            let advance = scaled.h_advance(id);
            if id.0 == 0 && !c.is_whitespace() {
                let h = style.size * 0.7;
                self.stroke_rect(caret + 1.0, baseline - h, (advance - 2.0).max(1.0), h, 1.0, style.color);
            } else if let Some(outline) =
                font.outline_glyph(id.with_scale_and_position(style.size, point(caret, baseline)))
            {
                let bounds = outline.px_bounds();
                let (ox, oy) = (bounds.min.x as i64, bounds.min.y as i64);
                outline.draw(|gx, gy, coverage| {
                    self.blend(ox + i64::from(gx), oy + i64::from(gy), style.color, coverage);
                });
            }
            caret += advance;
            last = Some(id);
        }
        caret - x
    }

    fn draw_boxes(&mut self, style: TextStyle, x: f32, y: f32, text: &str) -> f32 {
        let advance = box_advance(style.size);
        let h = style.size * 0.7;
        let top = y + style.size * 0.25;
        let mut caret = x;
        for c in text.chars() {
            if !c.is_whitespace() {
                self.stroke_rect(caret + 1.0, top, (advance - 2.0).max(1.0), h, 1.0, style.color);
            }
            caret += advance;
        }
        caret - x
    }

    pub fn into_rgba(self) -> Rgba {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dark_pixels(canvas: &Canvas) -> usize {
        (0..canvas.height())
            .flat_map(|y| (0..canvas.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| canvas.pixel(x, y).is_some_and(|p| p[0] < 128))
            .count()
    }

    #[test]
    fn text_without_a_font_still_shows_where_characters_are() {
        let mut canvas = Canvas::new(200, 40, Color::WHITE);
        let advance = canvas.draw_text(&Fonts::none(), TextStyle::new(20.0), 0.0, 0.0, "ab c");
        assert_eq!(advance, 4.0 * box_advance(20.0));
        assert!(dark_pixels(&canvas) > 0);
        let blank_column = (box_advance(20.0) * 2.5) as u32;
        assert!((0..40).all(|y| canvas.pixel(blank_column, y) == Some([255; 4])));
    }

    #[test]
    fn system_font_draws_glyphs_when_one_is_installed() {
        let fonts = Fonts::system();
        if !fonts.has_font() {
            return;
        }
        let mut canvas = Canvas::new(200, 40, Color::WHITE);
        canvas.draw_text(&fonts, TextStyle::new(24.0), 2.0, 2.0, "Hello");
        assert!(dark_pixels(&canvas) > 20);
        assert!(fonts.text_width("Hello world", 24.0, false) > fonts.text_width("Hello", 24.0, false));
    }

    #[test]
    fn wrap_breaks_between_words_and_at_newlines() {
        let fonts = Fonts::none();
        let w = box_advance(10.0);
        let lines = fonts.wrap("aaa bbb ccc\nddd", 10.0, false, w * 7.5);
        assert_eq!(lines, ["aaa bbb", "ccc", "ddd"]);
        let long = fonts.wrap("abcdefghij", 10.0, false, w * 3.0);
        assert_eq!(long, ["abcdefghij"]);
    }

    #[test]
    fn blit_scales_the_source_into_the_rectangle() {
        let src = Rgba { width: 2, height: 1, data: vec![255, 0, 0, 255, 0, 0, 255, 255] };
        let mut canvas = Canvas::new(10, 10, Color::WHITE);
        canvas.blit(&src, 0.0, 0.0, 10.0, 10.0);
        assert_eq!(canvas.pixel(1, 5), Some([255, 0, 0, 255]));
        assert_eq!(canvas.pixel(8, 5), Some([0, 0, 255, 255]));
    }
}
