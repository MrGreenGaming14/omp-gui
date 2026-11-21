//! PDF pages rendered and read with `hayro`.

use crate::docs::raster::Fonts;
use crate::docs::{DataQuery, DocsError, Format, PageSize, Paged, RenderOpts, Result, Rgba, json, page_at};
use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, SoftMask, interpret_page,
};
use hayro::hayro_syntax::page::Page;
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::kurbo::{Affine, BezPath, Point, Rect};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::peniko::ImageAlphaType;
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::{RenderCache, RenderSettings, render_into};
use std::ops::Range;

pub struct PdfDoc {
    pdf: Pdf,
}

impl PdfDoc {
    /// Parses a PDF file. Encrypted files that need a password are rejected.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        Pdf::new(bytes.to_vec()).map(|pdf| Self { pdf }).map_err(|e| match e {
            LoadPdfError::Decryption(e) => DocsError::invalid(Format::Pdf, format!("cannot decrypt the document ({e:?})")),
            LoadPdfError::Invalid => DocsError::invalid(Format::Pdf, "not a readable PDF document"),
        })
    }

    /// Text of every page, one entry per page.
    pub fn text(&self) -> Vec<String> {
        let cache = InterpreterCache::new();
        self.pdf.pages().iter().map(|page| extract_text(page, &cache)).collect()
    }
}

impl Paged for PdfDoc {
    fn page_count(&self) -> usize {
        self.pdf.pages().len()
    }

    fn page_size(&self, index: usize) -> Result<PageSize> {
        let (width, height) = page_at(self.pdf.pages(), index)?.render_dimensions();
        Ok(PageSize { width, height })
    }

    fn query(&self, query: &DataQuery) -> Option<Result<serde_json::Value>> {
        Some(match query {
            DataQuery::Text { page: Some(i) } => {
                page_at(self.pdf.pages(), *i).and_then(|page| json(&extract_text(page, &InterpreterCache::new())))
            }
            DataQuery::Text { page: None } => json(&self.text()),
            _ => return None,
        })
    }

    fn render(&self, index: usize, scale: f32, _: &Fonts, _: &RenderOpts) -> Result<Rgba> {
        let (page, size) = (page_at(self.pdf.pages(), index)?, self.page_size(index)?);
        if !(scale.is_finite() && scale > 0.0) {
            return Err(DocsError::Request(format!("invalid render scale {scale}")));
        }
        let (width, height) = size.pixels(scale);
        let (Ok(width), Ok(height)) = (u16::try_from(width), u16::try_from(height)) else {
            let max = u16::MAX;
            let message = format!("page {index} at scale {scale} is {width} x {height} pixels, the limit is {max} x {max}");
            return Err(DocsError::Request(message));
        };

        let cache = RenderCache::new();
        let mut ctx = RenderContext::new(width, height);
        let transform = Affine::scale(f64::from(scale)) * page.initial_transform(true).to_kurbo();
        render_into(page, &cache, &InterpreterSettings::default(), &RenderSettings::default(), &mut ctx, transform);
        ctx.flush();
        let mut pixmap = Pixmap::new(width, height);
        ctx.render_with(
            &mut pixmap,
            &mut Resources::default(),
            RasterizerSettings { target_init: TargetInit::Clear(WHITE), ..Default::default() },
        );
        Ok(Rgba { width: u32::from(width), height: u32::from(height), data: pixmap.take_rgba8(ImageAlphaType::Alpha) })
    }
}

/// A placed glyph: in device space (points, y down, page rotation applied) while collecting, then
/// in reading space (`x` along the line, `y` down the page) during layout.
struct PlacedGlyph {
    origin: Point,
    /// Where the next glyph would start without extra spacing.
    end: Point,
    /// Font size in device units.
    size: f64,
    /// Baseline direction in quarter turns clockwise from left to right.
    quadrant: usize,
    /// The glyph's characters in [`TextCollector::text`].
    text: Range<usize>,
}

/// A device that records the glyphs a page shows, with their unicode text.
#[derive(Default)]
struct TextCollector {
    text: String,
    glyphs: Vec<PlacedGlyph>,
    /// Glyph count and first origin of the last filled run, to skip the stroke pass of
    /// fill-and-stroke text, which repeats the same glyphs.
    last_fill: Option<(usize, Point)>,
}

impl<'a> Device<'a> for TextCollector {
    fn draw_glyph_run(&mut self, glyph_run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, draw_mode: &DrawMode) {
        let glyphs = glyph_run.glyphs();
        let Some(first) = glyphs.first() else { return };
        let first_origin = (props.transform * first.transform()) * Point::ZERO;
        let stroke = matches!(draw_mode, DrawMode::Stroke(_));
        if stroke && self.last_fill == Some((glyphs.len(), first_origin)) {
            return;
        }
        self.last_fill = (!stroke).then_some((glyphs.len(), first_origin));

        for positioned in glyphs {
            let glyph: &Glyph<'a> = positioned;
            let Some(unicode) = glyph.as_unicode() else { continue };
            let start = self.text.len();
            match unicode {
                BfString::Char(c) => self.text.push(c),
                BfString::String(s) => self.text.push_str(&s),
            }
            let advance = match glyph {
                Glyph::Outline(g) => g.advance_width(),
                Glyph::Type3(g) => g.advance_width(),
            };
            // Glyph space is 1000 units per em, so the transformed vertical unit vector times 1000 is
            // the font size on the page.
            let full = props.transform * positioned.transform();
            let [a, b, c, d, _, _] = full.as_coeffs();
            self.glyphs.push(PlacedGlyph {
                origin: full * Point::ZERO,
                end: full * Point::new(advance.map_or(500.0, f64::from), 0.0),
                size: c.hypot(d) * 1000.0,
                quadrant: ((b.atan2(a) / std::f64::consts::FRAC_PI_2).round() as i64).rem_euclid(4) as usize,
                text: start..self.text.len(),
            });
        }
    }

    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

/// Fraction of the font size two glyphs' baselines may differ by and still share a line.
const LINE_TOLERANCE: f64 = 0.4;
/// Fraction of the font size a horizontal gap must exceed to count as a space.
const SPACE_GAP: f64 = 0.15;

/// The page's text in reading order: lines top to bottom joined with `\n`, words separated by a space.
/// Invisible text (OCR layers) is included.
fn extract_text<'a>(page: &Page<'a>, cache: &InterpreterCache<'a>) -> String {
    let (width, height) = page.render_dimensions();
    let mut collector = TextCollector::default();
    let mut context = Context::new(
        page.initial_transform(true).to_kurbo(),
        Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
        cache,
        page.xref(),
        InterpreterSettings::default(),
    );
    interpret_page(page, &mut context, &mut collector);
    let TextCollector { text, mut glyphs, .. } = collector;

    // Read along the direction most glyphs run in, so rotated text still comes out line by line.
    let count = |q: usize| glyphs.iter().filter(|g| g.quadrant == q).count();
    let quadrant = (0..4).max_by_key(|&q| (count(q), std::cmp::Reverse(q))).unwrap_or(0);
    let (sin, cos) = [(0.0, 1.0), (1.0, 0.0), (0.0, -1.0), (-1.0, 0.0)][quadrant];
    let rotate = |p: Point| Point::new(p.x * cos + p.y * sin, p.y * cos - p.x * sin);
    glyphs.iter_mut().for_each(|g| (g.origin, g.end) = (rotate(g.origin), rotate(g.end)));
    glyphs.sort_by(|a, b| a.origin.y.total_cmp(&b.origin.y).then(a.origin.x.total_cmp(&b.origin.x)));

    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let mut start = 0;
    while start < glyphs.len() {
        let base = glyphs[start].origin.y;
        let (mut line_size, mut end) = (glyphs[start].size, start + 1);
        while end < glyphs.len() && glyphs[end].origin.y - base <= LINE_TOLERANCE * line_size.max(glyphs[end].size) {
            line_size = line_size.max(glyphs[end].size);
            end += 1;
        }
        let line = &mut glyphs[start..end];
        line.sort_by(|a, b| a.origin.x.total_cmp(&b.origin.x));

        let line_start = out.len();
        if line_start > 0 {
            out.push('\n');
        }
        let mut prev: Option<&PlacedGlyph> = None;
        for glyph in line.iter() {
            let glyph_text = &text[glyph.text.clone()];
            if prev.is_some_and(|prev| {
                glyph.origin.x - prev.end.x > SPACE_GAP * prev.size.max(glyph.size)
                    && !out.ends_with(char::is_whitespace)
                    && !glyph_text.starts_with(char::is_whitespace)
            }) {
                out.push(' ');
            }
            out.push_str(glyph_text);
            prev = Some(glyph);
        }
        out.truncate(out.trim_end().len().max(line_start));
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PDF of `(width, height, rotate, content)` pages with a Helvetica `/F1` and correct xref offsets.
    fn build_pdf(pages: &[(f32, f32, i32, &str)]) -> Vec<u8> {
        let kids: Vec<String> = (0..pages.len()).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        for (i, (w, h, rotate, content)) in pages.iter().enumerate() {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Rotate {rotate} /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                5 + 2 * i
            ));
            objects.push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len() + 1));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut xref = format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
        for (i, object) in objects.iter().enumerate() {
            xref.push_str(&format!("{:010} 00000 n \n", out.len()));
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
        }
        let start = out.len();
        out.extend_from_slice(format!("{xref}trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n", objects.len() + 1).as_bytes());
        out
    }

    #[test]
    fn a_pdf_round_trips_to_sizes_reading_order_text_and_pixels() {
        // Lower lines come first to check ordering; "Split words" is placed word by word, "Kern" uses TJ.
        let first = "BT /F1 12 Tf 72 600 Td (Second line) Tj ET BT /F1 12 Tf 72 500 Td (Split) Tj 40 0 Td (words) Tj ET \
                     BT /F1 12 Tf 72 450 Td [(Ke) -20 (rn)] TJ ET BT /F1 24 Tf 72 700 Td (Hello PDF) Tj ET 0 0 1 rg 100 100 50 50 re f";
        // Drawn rotated on a /Rotate 90 page, so it reads upright on screen.
        let rotated = "BT /F1 12 Tf 0 1 -1 0 280 100 Tm (Body text) Tj ET BT /F1 20 Tf 0 1 -1 0 250 100 Tm (Upright title) Tj ET \
                       1 0 0 rg 0 0 50 50 re f";
        let doc = PdfDoc::parse(&build_pdf(&[(612.0, 792.0, 0, first), (842.0, 595.0, 0, ""), (612.0, 792.0, 90, rotated)])).unwrap();
        let sizes: Vec<_> = (0..3).map(|i| doc.page_size(i).unwrap()).map(|s| (s.width, s.height)).collect();
        assert_eq!(sizes, [(612.0, 792.0), (842.0, 595.0), (792.0, 612.0)]);
        assert_eq!(doc.text(), ["Hello PDF\nSecond line\nSplit words\nKern", "", "Upright title\nBody text"]);

        let image = doc.render_page(0, 2.0, &Fonts::none()).unwrap();
        assert_eq!((image.width, image.height), (1224, 1584));
        // PDF y 100..150 is device y (792 - 150)..(792 - 100), doubled.
        assert_eq!(image.pixel(250, 1334), Some([0, 0, 255, 255]));
        assert_eq!(image.pixel(350, 1334), Some([255, 255, 255, 255]));
        let text_ink = (144..360).flat_map(|x| (144..184).map(move |y| (x, y))).filter(|&(x, y)| image.pixel(x, y).unwrap()[0] < 128);
        assert!(text_ink.count() > 50);
        // Rotating 90 degrees clockwise moves the bottom left corner to the top left.
        let image = doc.render_page(2, 1.0, &Fonts::none()).unwrap();
        assert_eq!((image.width, image.height, image.pixel(25, 25)), (792, 612, Some([255, 0, 0, 255])));
        assert_eq!(doc.render_page(0, 1.3, &Fonts::none()).map(|i| (i.width, i.height)).unwrap(), (796, 1030));

        let page_text = doc.query(&DataQuery::Text { page: Some(3) });
        assert!(matches!(page_text, Some(Err(DocsError::PageRange { index: 3, count: 3 }))));
        assert!(matches!(doc.render_page(0, 200.0, &Fonts::none()), Err(DocsError::Request(_))));
        assert!(matches!(doc.render_page(0, 0.0, &Fonts::none()), Err(DocsError::Request(_))));
        assert!(matches!(PdfDoc::parse(b"not a pdf"), Err(DocsError::Invalid { format: Format::Pdf, .. })));
    }
}
