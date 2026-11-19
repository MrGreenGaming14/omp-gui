//! PDF pages rendered and read with `hayro`.

use crate::raster::Fonts;
use crate::{DocsError, Format, PageSize, Paged, Result, Rgba};
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

/// A parsed PDF document with its page sizes measured once at load.
pub struct PdfDoc {
    pdf: Pdf,
    sizes: Vec<PageSize>,
}

impl PdfDoc {
    /// Parses a PDF file. Encrypted files that need a password are rejected.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let pdf = Pdf::new(bytes.to_vec()).map_err(|e| {
            let message = match e {
                LoadPdfError::Decryption(e) => format!("cannot decrypt the document ({e:?})"),
                LoadPdfError::Invalid => "not a readable PDF document".to_owned(),
            };
            DocsError::invalid(Format::Pdf, message)
        })?;
        let sizes = pdf
            .pages()
            .iter()
            .map(|page| {
                let (width, height) = page.render_dimensions();
                PageSize { width, height }
            })
            .collect();
        Ok(Self { pdf, sizes })
    }

    /// Text of page `index` in reading order: lines top to bottom joined with
    /// `\n`, words separated by a space. Invisible text (OCR layers) is
    /// included.
    pub fn page_text(&self, index: usize) -> Result<String> {
        let page = self.page(index)?;
        Ok(extract_text(page, &InterpreterCache::new()))
    }

    /// Text of every page, one entry per page.
    pub fn text(&self) -> Result<Vec<String>> {
        let cache = InterpreterCache::new();
        Ok(self.pdf.pages().iter().map(|page| extract_text(page, &cache)).collect())
    }

    fn page(&self, index: usize) -> Result<&Page<'_>> {
        let pages = self.pdf.pages();
        pages.get(index).ok_or_else(|| DocsError::page_range(index, pages.len()))
    }
}

impl Paged for PdfDoc {
    fn page_count(&self) -> usize {
        self.sizes.len()
    }

    fn page_size(&self, index: usize) -> Result<PageSize> {
        self.sizes
            .get(index)
            .copied()
            .ok_or_else(|| DocsError::page_range(index, self.sizes.len()))
    }

    fn render_page(&self, index: usize, scale: f32, _fonts: &Fonts) -> Result<Rgba> {
        let page = self.page(index)?;
        let size = self.page_size(index)?;
        if !(scale.is_finite() && scale > 0.0) {
            return Err(DocsError::Request(format!("invalid render scale {scale}")));
        }
        let (width, height) = size.pixels(scale);
        let (Ok(width), Ok(height)) = (u16::try_from(width), u16::try_from(height)) else {
            return Err(DocsError::Request(format!(
                "page {index} at scale {scale} is {width} x {height} pixels, the limit is {max} x {max}",
                max = u16::MAX
            )));
        };

        let cache = RenderCache::new();
        let mut ctx = RenderContext::new(width, height);
        let transform = Affine::scale(f64::from(scale)) * page.initial_transform(true).to_kurbo();
        render_into(
            page,
            &cache,
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &mut ctx,
            transform,
        );
        ctx.flush();
        let mut pixmap = Pixmap::new(width, height);
        ctx.render_with(
            &mut pixmap,
            &mut Resources::default(),
            RasterizerSettings { target_init: TargetInit::Clear(WHITE), ..Default::default() },
        );
        Ok(Rgba {
            width: u32::from(width),
            height: u32::from(height),
            data: pixmap.take_rgba8(ImageAlphaType::Alpha),
        })
    }
}

/// A glyph placed in device space (points, y down, page rotation applied).
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
    /// Glyph count and first origin of the last filled run, to skip the
    /// stroke pass of fill-and-stroke text, which repeats the same glyphs.
    last_fill: Option<(usize, Point)>,
}

impl<'a> Device<'a> for TextCollector {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

    fn draw_glyph_run(
        &mut self,
        glyph_run: &GlyphRun<'_, 'a>,
        props: DrawProps<'a>,
        draw_mode: &DrawMode,
    ) {
        let glyphs = glyph_run.glyphs();
        let Some(first) = glyphs.first() else {
            return;
        };
        let first_origin = (props.transform * first.transform()) * Point::ZERO;
        let stroke = matches!(draw_mode, DrawMode::Stroke(_));
        if stroke && self.last_fill == Some((glyphs.len(), first_origin)) {
            return;
        }
        self.last_fill = (!stroke).then_some((glyphs.len(), first_origin));

        for positioned in glyphs {
            let glyph: &Glyph<'a> = positioned;
            let Some(unicode) = glyph.as_unicode() else {
                continue;
            };
            let start = self.text.len();
            match unicode {
                BfString::Char(c) => self.text.push(c),
                BfString::String(s) => self.text.push_str(&s),
            }
            let advance = match glyph {
                Glyph::Outline(g) => g.advance_width(),
                Glyph::Type3(g) => g.advance_width(),
            };
            // Glyph space is 1000 units per em, so the transformed vertical
            // unit vector times 1000 is the font size on the page.
            let full = props.transform * positioned.transform();
            let [a, b, c, d, _, _] = full.as_coeffs();
            let size = c.hypot(d) * 1000.0;
            let quadrant = ((b.atan2(a) / std::f64::consts::FRAC_PI_2).round() as i64)
                .rem_euclid(4) as usize;
            let advance = advance.map_or(500.0, f64::from);
            self.glyphs.push(PlacedGlyph {
                origin: full * Point::ZERO,
                end: full * Point::new(advance, 0.0),
                size,
                quadrant,
                text: start..self.text.len(),
            });
        }
    }

    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}
}

/// A glyph in reading coordinates: `u` grows along the line, `v` down the page.
struct ReadGlyph {
    u: f64,
    end_u: f64,
    v: f64,
    size: f64,
    text: Range<usize>,
}

/// Fraction of the font size two glyphs' baselines may differ by and still
/// share a line.
const LINE_TOLERANCE: f64 = 0.4;
/// Fraction of the font size a horizontal gap must exceed to count as a space.
const SPACE_GAP: f64 = 0.15;

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
    layout(&collector)
}

/// Orders collected glyphs into lines and joins them into text.
fn layout(collector: &TextCollector) -> String {
    if collector.glyphs.is_empty() {
        return String::new();
    }
    // Read along the direction most glyphs run in, so text on rotated pages
    // still comes out line by line.
    let mut counts = [0usize; 4];
    for glyph in &collector.glyphs {
        counts[glyph.quadrant] += 1;
    }
    let mut quadrant = 0;
    for (q, &count) in counts.iter().enumerate() {
        if count > counts[quadrant] {
            quadrant = q;
        }
    }
    let (sin, cos) = [(0.0, 1.0), (1.0, 0.0), (0.0, -1.0), (-1.0, 0.0)][quadrant];
    let to_u = |p: Point| p.x * cos + p.y * sin;
    let to_v = |p: Point| p.y * cos - p.x * sin;

    let mut glyphs: Vec<ReadGlyph> = collector
        .glyphs
        .iter()
        .map(|g| ReadGlyph {
            u: to_u(g.origin),
            end_u: to_u(g.end),
            v: to_v(g.origin),
            size: g.size,
            text: g.text.clone(),
        })
        .collect();
    glyphs.sort_by(|a, b| a.v.total_cmp(&b.v).then(a.u.total_cmp(&b.u)));

    let mut out = String::with_capacity(collector.text.len() + collector.text.len() / 8);
    let mut start = 0;
    while start < glyphs.len() {
        let base_v = glyphs[start].v;
        let mut line_size = glyphs[start].size;
        let mut end = start + 1;
        while end < glyphs.len()
            && glyphs[end].v - base_v <= LINE_TOLERANCE * line_size.max(glyphs[end].size)
        {
            line_size = line_size.max(glyphs[end].size);
            end += 1;
        }
        let line = &mut glyphs[start..end];
        line.sort_by(|a, b| a.u.total_cmp(&b.u));

        let line_start = out.len();
        if line_start > 0 {
            out.push('\n');
        }
        let mut prev: Option<&ReadGlyph> = None;
        for glyph in line.iter() {
            let text = &collector.text[glyph.text.clone()];
            if let Some(prev) = prev {
                let gap = glyph.u - prev.end_u;
                if gap > SPACE_GAP * prev.size.max(glyph.size)
                    && !out.ends_with(char::is_whitespace)
                    && !text.starts_with(char::is_whitespace)
                {
                    out.push(' ');
                }
            }
            out.push_str(text);
            prev = Some(glyph);
        }
        let trimmed = out.trim_end().len().max(line_start);
        out.truncate(trimmed);
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One page of a test document.
    struct TestPage<'a> {
        width: f32,
        height: f32,
        rotate: i32,
        content: &'a str,
    }

    impl<'a> TestPage<'a> {
        fn new(width: f32, height: f32, content: &'a str) -> Self {
            Self { width, height, rotate: 0, content }
        }
    }

    /// Builds a PDF with a Helvetica font resource and correct xref offsets.
    fn build_pdf(pages: &[TestPage<'_>]) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        let kids: Vec<String> =
            (0..pages.len()).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_owned());
        objects.push(format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            pages.len()
        ));
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned());
        for (i, page) in pages.iter().enumerate() {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Rotate {} \
                 /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                page.width,
                page.height,
                page.rotate,
                5 + 2 * i
            ));
            objects.push(format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                page.content.len() + 1,
                page.content
            ));
        }

        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (i, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn pixel(image: &Rgba, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * image.width + x) * 4) as usize;
        [image.data[i], image.data[i + 1], image.data[i + 2], image.data[i + 3]]
    }

    const WHITE_PX: [u8; 4] = [255, 255, 255, 255];

    #[test]
    fn page_count_and_sizes_follow_media_box_and_rotation() {
        let rotated = TestPage { rotate: 90, ..TestPage::new(612.0, 792.0, "") };
        let doc = PdfDoc::parse(&build_pdf(&[
            TestPage::new(612.0, 792.0, ""),
            TestPage::new(842.0, 595.0, ""),
            rotated,
        ]))
        .unwrap();
        assert_eq!(doc.page_count(), 3);
        assert_eq!(doc.page_size(0).unwrap(), PageSize { width: 612.0, height: 792.0 });
        assert_eq!(doc.page_size(1).unwrap(), PageSize { width: 842.0, height: 595.0 });
        assert_eq!(doc.page_size(2).unwrap(), PageSize { width: 792.0, height: 612.0 });
    }

    #[test]
    fn text_is_in_reading_order_per_page() {
        // The lower lines are written first to check top to bottom ordering;
        // "Split words" is positioned word by word, "Kern" is kerned with TJ.
        let first = "BT /F1 12 Tf 72 600 Td (Second line) Tj ET \
                     BT /F1 12 Tf 72 500 Td (Split) Tj 40 0 Td (words) Tj ET \
                     BT /F1 12 Tf 72 450 Td [(Ke) -20 (rn)] TJ ET \
                     BT /F1 24 Tf 72 700 Td (Hello PDF) Tj ET";
        let second = "BT /F1 18 Tf 72 700 Td (Page two) Tj ET \
                      BT 3 Tr /F1 12 Tf 72 650 Td (Hidden layer) Tj ET";
        let doc = PdfDoc::parse(&build_pdf(&[
            TestPage::new(612.0, 792.0, first),
            TestPage::new(612.0, 792.0, second),
        ]))
        .unwrap();
        assert_eq!(doc.page_text(0).unwrap(), "Hello PDF\nSecond line\nSplit words\nKern");
        assert_eq!(
            doc.text().unwrap(),
            vec![
                "Hello PDF\nSecond line\nSplit words\nKern".to_owned(),
                "Page two\nHidden layer".to_owned(),
            ]
        );
    }

    #[test]
    fn text_on_rotated_page_reads_along_the_rotated_lines() {
        // Content drawn rotated 90 degrees counterclockwise on a /Rotate 90
        // page reads upright on screen; lines further down the screen sit at
        // larger PDF x.
        let content = "BT /F1 12 Tf 0 1 -1 0 280 100 Tm (Body text) Tj ET \
                       BT /F1 20 Tf 0 1 -1 0 250 100 Tm (Upright title) Tj ET";
        let page = TestPage { rotate: 90, ..TestPage::new(612.0, 792.0, content) };
        let doc = PdfDoc::parse(&build_pdf(&[page])).unwrap();
        assert_eq!(doc.page_text(0).unwrap(), "Upright title\nBody text");
    }

    #[test]
    fn render_fills_rectangle_at_flipped_position_on_white() {
        let content = "0 0 1 rg 100 100 50 50 re f";
        let doc = PdfDoc::parse(&build_pdf(&[TestPage::new(612.0, 792.0, content)])).unwrap();
        let image = doc.render_page(0, 2.0, &Fonts::none()).unwrap();
        assert_eq!((image.width, image.height), (1224, 1584));
        assert_eq!(image.data.len(), 1224 * 1584 * 4);
        // PDF y 100..150 is device y (792 - 150)..(792 - 100), doubled.
        assert_eq!(pixel(&image, 250, 1334), [0, 0, 255, 255]);
        assert_eq!(pixel(&image, 250, 250), WHITE_PX);
        assert_eq!(pixel(&image, 10, 10), WHITE_PX);
        assert_eq!(pixel(&image, 350, 1334), WHITE_PX);
    }

    #[test]
    fn render_size_rounds_scaled_page_size() {
        let doc = PdfDoc::parse(&build_pdf(&[TestPage::new(612.0, 792.0, "")])).unwrap();
        let image = doc.render_page(0, 1.3, &Fonts::none()).unwrap();
        assert_eq!((image.width, image.height), (796, 1030));
    }

    #[test]
    fn render_of_rotated_page_swaps_dimensions_and_moves_content() {
        let content = "1 0 0 rg 0 0 50 50 re f";
        let page = TestPage { rotate: 90, ..TestPage::new(612.0, 792.0, content) };
        let doc = PdfDoc::parse(&build_pdf(&[page])).unwrap();
        let image = doc.render_page(0, 1.0, &Fonts::none()).unwrap();
        assert_eq!((image.width, image.height), (792, 612));
        // Rotating 90 degrees clockwise moves the bottom left corner to the
        // top left of the displayed page.
        assert_eq!(pixel(&image, 25, 25), [255, 0, 0, 255]);
        assert_eq!(pixel(&image, 25, 587), WHITE_PX);
    }

    #[test]
    fn render_draws_text() {
        let content = "BT /F1 24 Tf 72 700 Td (Hello PDF) Tj ET";
        let doc = PdfDoc::parse(&build_pdf(&[TestPage::new(612.0, 792.0, content)])).unwrap();
        let image = doc.render_page(0, 1.0, &Fonts::none()).unwrap();
        // The baseline is at device y 92 and capitals rise about 17 points.
        let dark = (72..180)
            .flat_map(|x| (72..92).map(move |y| (x, y)))
            .filter(|&(x, y)| pixel(&image, x, y)[0] < 128)
            .count();
        assert!(dark > 50, "only {dark} dark pixels where the text is");
        assert_eq!(pixel(&image, 300, 400), WHITE_PX);
    }

    #[test]
    fn out_of_range_page_is_an_error() {
        let doc = PdfDoc::parse(&build_pdf(&[TestPage::new(612.0, 792.0, "")])).unwrap();
        let in_range = |r: &DocsError| matches!(r, DocsError::PageRange { index: 1, count: 1 });
        assert!(in_range(&doc.page_size(1).unwrap_err()));
        assert!(in_range(&doc.render_page(1, 1.0, &Fonts::none()).unwrap_err()));
        assert!(in_range(&doc.page_text(1).unwrap_err()));
    }

    #[test]
    fn oversized_render_is_a_request_error() {
        let doc = PdfDoc::parse(&build_pdf(&[TestPage::new(612.0, 792.0, "")])).unwrap();
        assert!(matches!(
            doc.render_page(0, 200.0, &Fonts::none()),
            Err(DocsError::Request(_))
        ));
        assert!(matches!(doc.render_page(0, 0.0, &Fonts::none()), Err(DocsError::Request(_))));
    }

    #[test]
    fn garbage_is_invalid_pdf() {
        let result = PdfDoc::parse(b"this is not a pdf file at all");
        assert!(matches!(result, Err(DocsError::Invalid { format: Format::Pdf, .. })));
    }
}
