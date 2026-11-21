//! PPTX presentations: slide text and shape geometry for the agent, and a simple slide renderer.

use crate::docs::ooxml::{Package, XmlResult, attr, find_attr, num_attr, xml_reader};
use crate::docs::raster::{Canvas, Fonts};
use crate::docs::{DataQuery, Format, PageSize, Paged, RenderOpts, Result, Rgba, json, page_at};
use quick_xml::events::{BytesStart, Event};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;

/// English Metric Units per point.
const EMU_PER_POINT: f64 = 12700.0;
/// Slide size when `p:sldSz` is missing (4:3).
const DEFAULT_SLIDE_SIZE: PageSize = PageSize { width: 720.0, height: 540.0 };
/// Default left/right and top text insets of a text body, in points.
const INSET_X: f32 = 7.2;
const INSET_Y: f32 = 3.6;

const PRESENTATION_PART: &str = "ppt/presentation.xml";
const REL_SLIDE: &str = "/slide";
const REL_NOTES: &str = "/notesSlide";
const REL_LAYOUT: &str = "/slideLayout";
const REL_MASTER: &str = "/slideMaster";

/// A parsed presentation: slides in presentation order and the bytes of the pictures they show.
#[derive(Clone, Debug)]
pub struct PptxDoc {
    size: PageSize,
    slides: Vec<Slide>,
    /// Picture bytes by part path (`ppt/media/image1.png`).
    media: HashMap<String, Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Slide {
    /// Position in presentation order, from 0 (the page index).
    pub index: usize,
    /// Text of the first title or centered title placeholder.
    pub title: Option<String>,
    /// Non empty paragraphs (table rows for tables) of the other text shapes, in document order.
    pub body: Vec<String>,
    /// Text of the notes slide's body placeholder.
    pub notes: Option<String>,
    pub shapes: Vec<Shape>,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShapeKind {
    Text,
    Picture,
    Table,
    #[default]
    Other,
}

/// A shape on a slide. Geometry is in points from the slide's top left corner, after group
/// transforms; placeholders without their own geometry take it from the slide layout or master.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Shape {
    pub name: String,
    pub kind: ShapeKind,
    /// The `p:ph` type (`title`, `ctrTitle`, `body`, `subTitle`, `obj`, ...).
    pub placeholder: Option<String>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// One entry per paragraph, line breaks as `\n`. Tables give one entry per row, cells
    /// separated by ` | `.
    pub paragraphs: Vec<String>,
    /// Part path of the picture (`ppt/media/image1.png`).
    pub image: Option<String>,
    /// Drawing style of each entry of `paragraphs`.
    #[serde(skip)]
    styles: Vec<ParaStyle>,
}

/// How a paragraph is drawn, from its first run's properties.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct ParaStyle {
    /// Font size in points.
    size: Option<f32>,
    bold: bool,
}

impl PptxDoc {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut pkg = Package::open(bytes, Format::Pptx)?;
        let (size, order) = pkg
            .parsed(PRESENTATION_PART, parse_presentation)?
            .ok_or_else(|| pkg.invalid(format!("{PRESENTATION_PART} is missing")))?;
        let presentation_rels = pkg.rels(PRESENTATION_PART)?;
        let (mut templates, mut media) = (HashMap::new(), HashMap::new());
        let mut slides = Vec::with_capacity(order.len());
        for (index, rid) in order.iter().enumerate() {
            let part = presentation_rels
                .iter()
                .find(|r| r.id == *rid && r.kind.ends_with(REL_SLIDE))
                .map(|r| r.target.as_str())
                .ok_or_else(|| pkg.invalid(format!("slide relationship {rid} is missing")))?;
            slides.push(read_slide(&mut pkg, &mut templates, &mut media, index, part)?);
        }
        Ok(Self { size: size.unwrap_or(DEFAULT_SLIDE_SIZE), slides, media })
    }

    /// Plain text: per slide a `Slide N` header, the title, the body paragraphs and the notes.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for slide in &self.slides {
            if !out.is_empty() {
                out.push('\n');
            }
            let _ = writeln!(out, "Slide {}", slide.index + 1);
            for line in slide.title.iter().chain(&slide.body) {
                let _ = writeln!(out, "{line}");
            }
            if let Some(notes) = &slide.notes {
                let _ = writeln!(out, "Notes: {notes}");
            }
        }
        out
    }

    /// Draws a picture into its box, or a rule coloured box when it can't be decoded.
    fn draw_picture(&self, canvas: &mut Canvas, opts: &RenderOpts, path: Option<&str>, (x, y, w, h): Rect32) {
        let decoded = path.and_then(|p| self.media.get(p)).and_then(|bytes| ::image::load_from_memory(bytes).ok());
        match decoded {
            Some(img) => {
                let img = img.into_rgba8();
                canvas.blit(&Rgba { width: img.width(), height: img.height(), data: img.into_raw() }, x, y, w, h)
            }
            None => canvas.fill_rect(x, y, w, h, opts.palette.rule),
        }
    }
}

/// A box in points or pixels: x, y, width, height.
type Rect32 = (f32, f32, f32, f32);

impl Paged for PptxDoc {
    fn page_count(&self) -> usize {
        self.slides.len()
    }

    fn page_size(&self, index: usize) -> Result<PageSize> {
        page_at(&self.slides, index).map(|_| self.size)
    }

    fn query(&self, query: &DataQuery) -> Option<Result<serde_json::Value>> {
        Some(match query {
            DataQuery::Text { page: Some(index) } => page_at(&self.slides, *index).and_then(json),
            DataQuery::Text { page: None } => json(&self.text()),
            DataQuery::Slides => json(&self.slides),
            _ => return None,
        })
    }

    fn render(&self, index: usize, scale: f32, fonts: &Fonts, opts: &RenderOpts) -> Result<Rgba> {
        let slide = page_at(&self.slides, index)?;
        let (width, height) = self.size.pixels(scale);
        let mut canvas = Canvas::new(width, height, opts.palette.page);
        for shape in &slide.shapes {
            let is_title = is_title(shape.placeholder.as_deref());
            let Some((x, y, w, h)) = shape_box(shape, is_title, self.size) else { continue };
            let px = (x * scale, y * scale, w * scale, h * scale);
            match shape.kind {
                ShapeKind::Picture => self.draw_picture(&mut canvas, opts, shape.image.as_deref(), px),
                ShapeKind::Table => canvas.stroke_rect(px.0, px.1, px.2, px.3, scale, opts.palette.grid),
                ShapeKind::Text | ShapeKind::Other => {}
            }
            draw_paragraphs(&mut canvas, fonts, opts, shape, is_title, px, scale);
        }
        Ok(canvas.into_rgba())
    }
}

fn is_title(placeholder: Option<&str>) -> bool {
    matches!(placeholder, Some("title" | "ctrTitle"))
}

/// The box a shape is drawn in, in points. Placeholders whose geometry couldn't be resolved get
/// the usual title or body area.
fn shape_box(shape: &Shape, is_title: bool, slide: PageSize) -> Option<Rect32> {
    if shape.width > 0.0 && shape.height > 0.0 {
        return Some((shape.x, shape.y, shape.width, shape.height));
    }
    if shape.placeholder.is_none() || shape.paragraphs.is_empty() {
        return None;
    }
    let (w, h) = (slide.width, slide.height);
    Some(if is_title { (w * 0.05, h * 0.04, w * 0.9, h * 0.16) } else { (w * 0.05, h * 0.24, w * 0.9, h * 0.7) })
}

/// Draws a shape's paragraphs wrapped to its box, top aligned.
fn draw_paragraphs(
    canvas: &mut Canvas,
    fonts: &Fonts,
    opts: &RenderOpts,
    shape: &Shape,
    is_title: bool,
    (x, y, w, _): Rect32,
    scale: f32,
) {
    let left = x + INSET_X * scale;
    let max_width = (w - 2.0 * INSET_X * scale).max(1.0);
    let bottom = canvas.height() as f32;
    let mut top = y + INSET_Y * scale;
    for (i, paragraph) in shape.paragraphs.iter().enumerate() {
        let style = shape.styles.get(i).copied().unwrap_or_default();
        let size = style.size.unwrap_or(opts.typography.slide) * scale;
        let bold = style.bold || is_title;
        let text_style = opts.text(size).bold(bold);
        for line in fonts.wrap(paragraph, size, bold, max_width) {
            if top >= bottom {
                return;
            }
            canvas.draw_text(fonts, text_style, left, top, line);
            top += fonts.line_height(size);
        }
    }
}

/// Reads one slide part with its pictures and notes.
fn read_slide(
    pkg: &mut Package<'_>,
    templates: &mut HashMap<String, Template>,
    media: &mut HashMap<String, Vec<u8>>,
    index: usize,
    part: &str,
) -> Result<Slide> {
    let parsed =
        pkg.parsed(part, parse_shapes)?.ok_or_else(|| pkg.invalid(format!("slide part {part} is missing")))?;
    let rels = pkg.rels(part)?;
    let layout = rels.iter().find(|r| r.kind.ends_with(REL_LAYOUT)).map(|r| r.target.as_str());
    if let Some(layout) = layout {
        load_template(pkg, templates, layout)?;
    }

    let mut shapes = Vec::with_capacity(parsed.len());
    for p in parsed {
        let rect = p.rect.or_else(|| inherited_rect(templates, layout, p.placeholder.as_deref()?, p.ph_idx.as_deref()));
        let image = p.embed.as_deref().and_then(|id| rels.iter().find(|r| r.id == id)).map(|r| r.target.clone());
        if let Some(path) = &image
            && !media.contains_key(path)
            && let Some(bytes) = pkg.read(path)?
        {
            media.insert(path.clone(), bytes);
        }
        let rect = rect.unwrap_or_default();
        let pt = |emu: f64| (emu / EMU_PER_POINT) as f32;
        shapes.push(Shape {
            name: p.name,
            kind: p.kind,
            placeholder: p.placeholder,
            x: pt(rect.x),
            y: pt(rect.y),
            width: pt(rect.w),
            height: pt(rect.h),
            paragraphs: p.paragraphs,
            image,
            styles: p.styles,
        });
    }

    let title_at = shapes.iter().position(|s| is_title(s.placeholder.as_deref()));
    let title = title_at.and_then(|i| joined_text(shapes[i].paragraphs.iter()));
    let body = shapes
        .iter()
        .enumerate()
        .filter(|&(i, s)| {
            Some(i) != title_at
                && matches!(s.kind, ShapeKind::Text | ShapeKind::Table)
                && s.placeholder.as_deref() != Some("sldNum")
        })
        .flat_map(|(_, s)| s.paragraphs.iter().filter(|p| !p.trim().is_empty()).cloned())
        .collect();

    let notes = match rels.iter().find(|r| r.kind.ends_with(REL_NOTES)) {
        Some(rel) => pkg.parsed(&rel.target, parse_shapes)?.and_then(|parsed| {
            let body = parsed.iter().filter(|s| s.placeholder.as_deref() == Some("body"));
            joined_text(body.flat_map(|s| s.paragraphs.iter()))
        }),
        None => None,
    };
    Ok(Slide { index, title, body, notes, shapes })
}

/// Non empty paragraphs joined with newlines, `None` when there are none.
fn joined_text<'a>(paragraphs: impl Iterator<Item = &'a String>) -> Option<String> {
    let lines: Vec<&str> = paragraphs.map(String::as_str).filter(|p| !p.trim().is_empty()).collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Placeholder geometry of a slide layout or master.
struct Template {
    /// Placeholder type, index and geometry.
    placeholders: Vec<(String, Option<String>, Option<Rect>)>,
    parent: Option<String>,
}

/// Parses a layout or master part (and a layout's master) into `templates`.
fn load_template(pkg: &mut Package<'_>, templates: &mut HashMap<String, Template>, part: &str) -> Result<()> {
    if templates.contains_key(part) {
        return Ok(());
    }
    let placeholders = pkg
        .parsed(part, parse_shapes)?
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| Some((s.placeholder?, s.ph_idx, s.rect)))
        .collect();
    let parent = pkg.rels(part)?.into_iter().find(|r| r.kind.ends_with(REL_MASTER)).map(|r| r.target);
    // Inserted before the parent is loaded so a cycle of relationships ends.
    templates.insert(part.to_owned(), Template { placeholders, parent: parent.clone() });
    match parent {
        Some(parent) => load_template(pkg, templates, &parent),
        None => Ok(()),
    }
}

/// Placeholder types that inherit from the same master placeholder.
fn placeholder_family(kind: &str) -> &str {
    match kind {
        "ctrTitle" => "title",
        "subTitle" | "obj" => "body",
        other => other,
    }
}

/// Geometry of a placeholder from the layout (matched by index, then type) or its master
/// (matched by type).
fn inherited_rect(
    templates: &HashMap<String, Template>,
    layout: Option<&str>,
    kind: &str,
    idx: Option<&str>,
) -> Option<Rect> {
    let family = placeholder_family(kind);
    let (mut part, mut idx) = (layout, idx);
    while let Some(name) = part {
        let template = templates.get(name)?;
        let found = idx
            .and_then(|idx| template.placeholders.iter().find(|p| p.1.as_deref() == Some(idx)))
            .or_else(|| template.placeholders.iter().find(|p| placeholder_family(&p.0) == family));
        if let Some(rect) = found.and_then(|p| p.2) {
            return Some(rect);
        }
        part = template.parent.as_deref();
        idx = None;
    }
    None
}

/// Slide size and the slide relationship ids in presentation order.
fn parse_presentation(xml: &str) -> XmlResult<(Option<PageSize>, Vec<String>)> {
    let (mut size, mut order) = (None, Vec::new());
    crate::docs::ooxml::each_element(xml, |e| {
        match e.local_name().as_ref() {
            "sldSz" => {
                let (Some(cx), Some(cy)) = (num_attr(e, "cx")?, num_attr(e, "cy")?) else {
                    return Err("p:sldSz lacks cx or cy".into());
                };
                if cx <= 0.0 || cy <= 0.0 {
                    return Err(format!("slide size {cx} x {cy} EMU is not positive"));
                }
                let pt = |emu: f64| (emu / EMU_PER_POINT) as f32;
                size = Some(PageSize { width: pt(cx), height: pt(cy) });
            }
            "sldId" => {
                // The relationship id is the prefixed `r:id`; the bare `id` is a number.
                let rid = find_attr(e, |key| key.prefix().is_some() && key.local_name().as_ref() == "id")?;
                order.push(rid.ok_or("p:sldId lacks r:id")?.into_owned());
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok((size, order))
}

/// A rectangle in EMU.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// A group's transform from its child coordinate space to its parent's.
#[derive(Copy, Clone, Debug, Default)]
struct Frame {
    off: Rect,
    child: Rect,
}

impl Frame {
    fn apply(&self, r: Rect) -> Rect {
        let sx = if self.child.w > 0.0 { self.off.w / self.child.w } else { 1.0 };
        let sy = if self.child.h > 0.0 { self.off.h / self.child.h } else { 1.0 };
        Rect { x: self.off.x + (r.x - self.child.x) * sx, y: self.off.y + (r.y - self.child.y) * sy, w: r.w * sx, h: r.h * sy }
    }
}

/// A shape as read from a slide, layout, master or notes part.
#[derive(Default)]
struct ParsedShape {
    name: String,
    kind: ShapeKind,
    placeholder: Option<String>,
    ph_idx: Option<String>,
    /// Geometry on the slide, `None` when the shape has no `xfrm`.
    rect: Option<Rect>,
    paragraphs: Vec<String>,
    styles: Vec<ParaStyle>,
    /// Relationship id of the picture.
    embed: Option<String>,
}

/// The elements the shape parser tracks while they are open.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Tag {
    Group,
    Shape,
    Xfrm,
    Paragraph,
    Text,
    TableRow,
    TableCell,
    /// Content that is not read (`mc:Fallback`, which repeats `mc:Choice`).
    Skip,
    Other,
}

#[derive(Default)]
struct Builder {
    shape: ParsedShape,
    /// The element the shape was opened from (`sp`, `pic`, `graphicFrame`, `cxnSp`).
    element: &'static str,
    is_table: bool,
    in_paragraph: bool,
    paragraph: String,
    style: ParaStyle,
    style_from_run: bool,
    cell: Vec<String>,
    row: Vec<String>,
    row_style: Option<ParaStyle>,
}

/// Reads the shapes of a part's shape tree in document order, groups flattened.
fn parse_shapes(xml: &str) -> XmlResult<Vec<ParsedShape>> {
    let mut reader = xml_reader(xml);
    let mut parser = ShapeParser::default();
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => match parser.open(&e)? {
                Tag::Skip => {
                    reader.read_to_end(e.name()).map_err(|e| e.to_string())?;
                }
                tag => parser.stack.push(tag),
            },
            Event::Empty(e) => {
                let tag = parser.open(&e)?;
                parser.close(tag);
            }
            Event::End(_) => {
                if let Some(tag) = parser.stack.pop() {
                    parser.close(tag);
                }
            }
            Event::Text(t) => parser.push_text(&t.xml10_content()),
            Event::CData(t) => parser.push_text(&t),
            Event::GeneralRef(r) => match r.resolve_char_ref().map_err(|e| e.to_string())? {
                Some(c) => parser.push_text(c.encode_utf8(&mut [0; 4])),
                None => parser.push_text(
                    quick_xml::escape::resolve_predefined_entity(&r).ok_or_else(|| format!("unknown entity &{};", &*r))?,
                ),
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(parser.shapes)
}

/// What an opening element does to the parser, and what to track until it closes.
type Handler = fn(&mut ShapeParser, &BytesStart<'_>, &'static str) -> XmlResult<Tag>;

/// Opening element handlers by local name.
const HANDLERS: &[(&str, Handler)] = &[
    ("spTree", ShapeParser::group),
    ("grpSp", ShapeParser::group),
    ("sp", ShapeParser::shape),
    ("pic", ShapeParser::shape),
    ("graphicFrame", ShapeParser::shape),
    ("cxnSp", ShapeParser::shape),
    ("Fallback", |_, _, _| Ok(Tag::Skip)),
    ("xfrm", |_, _, _| Ok(Tag::Xfrm)),
    ("off", ShapeParser::geometry),
    ("ext", ShapeParser::geometry),
    ("chOff", ShapeParser::geometry),
    ("chExt", ShapeParser::geometry),
    ("cNvPr", ShapeParser::name),
    ("ph", ShapeParser::placeholder),
    ("blip", ShapeParser::blip),
    ("tbl", |p, _, _| p.edit(Tag::Other, |b| b.is_table = true)),
    ("tr", |_, _, _| Ok(Tag::TableRow)),
    ("tc", |p, _, _| p.edit(Tag::TableCell, |b| b.cell.clear())),
    ("p", ShapeParser::paragraph),
    ("t", |_, _, _| Ok(Tag::Text)),
    ("br", |p, _, _| p.edit(Tag::Other, |b| if b.in_paragraph { b.paragraph.push('\n') })),
    ("rPr", ShapeParser::run_props),
    ("endParaRPr", ShapeParser::run_props),
];

#[derive(Default)]
struct ShapeParser {
    stack: Vec<Tag>,
    /// Transforms of the open groups, the shape tree first.
    frames: Vec<Frame>,
    current: Option<Builder>,
    shapes: Vec<ParsedShape>,
}

impl ShapeParser {
    /// Appends character data to the paragraph when inside `a:t`.
    fn push_text(&mut self, text: &str) {
        if self.stack.last() == Some(&Tag::Text)
            && let Some(builder) = self.current.as_mut().filter(|b| b.in_paragraph)
        {
            builder.paragraph.push_str(text);
        }
    }

    /// Applies `f` to the open shape, if any, and tracks the element as `tag`.
    fn edit(&mut self, tag: Tag, f: impl FnOnce(&mut Builder)) -> XmlResult<Tag> {
        if let Some(builder) = self.current.as_mut() {
            f(builder);
        }
        Ok(tag)
    }

    fn open(&mut self, e: &BytesStart<'_>) -> XmlResult<Tag> {
        let local = e.local_name();
        let name: &str = local.as_ref();
        let found = HANDLERS.iter().find(|(n, _)| *n == name);
        found.map_or(Ok(Tag::Other), |&(n, handler)| handler(self, e, n))
    }

    fn group(&mut self, _: &BytesStart<'_>, _: &'static str) -> XmlResult<Tag> {
        if self.current.is_some() {
            return Ok(Tag::Other);
        }
        self.frames.push(Frame::default());
        Ok(Tag::Group)
    }

    fn shape(&mut self, _: &BytesStart<'_>, element: &'static str) -> XmlResult<Tag> {
        if self.current.is_some() {
            return Ok(Tag::Other);
        }
        self.current = Some(Builder { element, ..Builder::default() });
        Ok(Tag::Shape)
    }

    fn geometry(&mut self, e: &BytesStart<'_>, name: &'static str) -> XmlResult<Tag> {
        if self.stack.last() != Some(&Tag::Xfrm) {
            return Ok(Tag::Other);
        }
        let offset = name.ends_with("ff");
        let (kx, ky) = if offset { ("x", "y") } else { ("cx", "cy") };
        let (a, b) = (num_attr(e, kx)?.unwrap_or(0.0), num_attr(e, ky)?.unwrap_or(0.0));
        let own = matches!(name, "off" | "ext");
        let target = match &mut self.current {
            Some(builder) => own.then(|| builder.shape.rect.get_or_insert_with(Rect::default)),
            None => self.frames.last_mut().map(|f| if own { &mut f.off } else { &mut f.child }),
        };
        if let Some(rect) = target {
            if offset {
                (rect.x, rect.y) = (a, b);
            } else {
                (rect.w, rect.h) = (a, b);
            }
        }
        Ok(Tag::Other)
    }

    fn name(&mut self, e: &BytesStart<'_>, _: &'static str) -> XmlResult<Tag> {
        if let Some(builder) = self.current.as_mut().filter(|b| b.shape.name.is_empty())
            && let Some(name) = attr(e, "name")?
        {
            builder.shape.name = name.into_owned();
        }
        Ok(Tag::Other)
    }

    fn placeholder(&mut self, e: &BytesStart<'_>, _: &'static str) -> XmlResult<Tag> {
        if let Some(builder) = self.current.as_mut() {
            builder.shape.placeholder = Some(attr(e, "type")?.map_or_else(|| "obj".to_owned(), Cow::into_owned));
            builder.shape.ph_idx = attr(e, "idx")?.map(Cow::into_owned);
        }
        Ok(Tag::Other)
    }

    fn blip(&mut self, e: &BytesStart<'_>, _: &'static str) -> XmlResult<Tag> {
        if let Some(builder) = self.current.as_mut().filter(|b| b.shape.embed.is_none()) {
            builder.shape.embed = attr(e, "embed")?.map(Cow::into_owned);
        }
        Ok(Tag::Other)
    }

    fn paragraph(&mut self, _: &BytesStart<'_>, _: &'static str) -> XmlResult<Tag> {
        let Some(builder) = self.current.as_mut() else { return Ok(Tag::Other) };
        builder.in_paragraph = true;
        builder.paragraph.clear();
        builder.style = ParaStyle::default();
        builder.style_from_run = false;
        Ok(Tag::Paragraph)
    }

    fn run_props(&mut self, e: &BytesStart<'_>, name: &'static str) -> XmlResult<Tag> {
        if let Some(builder) = self.current.as_mut().filter(|b| b.in_paragraph && !b.style_from_run) {
            builder.style = ParaStyle {
                size: num_attr(e, "sz")?.map(|sz| (sz / 100.0) as f32).filter(|s| *s > 0.0),
                bold: attr(e, "b")?.is_some_and(|b| b == "1" || b == "true"),
            };
            builder.style_from_run = name == "rPr";
        }
        Ok(Tag::Other)
    }

    /// Handles the end of an element opened as `tag`.
    fn close(&mut self, tag: Tag) {
        if tag == Tag::Group {
            self.frames.pop();
            return;
        }
        if tag == Tag::Shape {
            if let Some(Builder { mut shape, element, is_table, .. }) = self.current.take() {
                shape.rect = shape.rect.map(|r| self.frames.iter().rev().fold(r, |r, f| f.apply(r)));
                let text = shape.placeholder.is_some() || shape.paragraphs.iter().any(|p| !p.trim().is_empty());
                shape.kind = match element {
                    "sp" if text => ShapeKind::Text,
                    "pic" => ShapeKind::Picture,
                    "graphicFrame" if is_table => ShapeKind::Table,
                    _ => ShapeKind::Other,
                };
                self.shapes.push(shape);
            }
            return;
        }
        let in_cell = self.stack.contains(&Tag::TableCell);
        let Some(b) = self.current.as_mut() else { return };
        match tag {
            Tag::Paragraph => {
                b.in_paragraph = false;
                let text = std::mem::take(&mut b.paragraph);
                if in_cell {
                    b.row_style.get_or_insert(b.style);
                    if !text.trim().is_empty() {
                        b.cell.push(text);
                    }
                } else {
                    b.shape.paragraphs.push(text);
                    b.shape.styles.push(b.style);
                }
            }
            Tag::TableCell => {
                let cell = b.cell.join(" ");
                b.row.push(cell);
                b.cell.clear();
            }
            Tag::TableRow => {
                b.shape.paragraphs.push(b.row.join(" | "));
                b.shape.styles.push(b.row_style.take().unwrap_or_default());
                b.row.clear();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::DocsError;
    use std::io::{Cursor, Write};

    const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

    fn zip_parts(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in parts {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn rels(entries: &[(&str, &str, &str)]) -> Vec<u8> {
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let body: String = entries
            .iter()
            .map(|(id, kind, target)| format!(r#"<Relationship Id="{id}" Type="{rel}/{kind}" Target="{target}"/>"#))
            .collect();
        format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{body}</Relationships>"#).into_bytes()
    }

    fn tree(root: &str, shapes: &str) -> Vec<u8> {
        format!(r#"<?xml version="1.0" encoding="UTF-8"?><p:{root} {NS}><p:cSld><p:spTree><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>{shapes}</p:spTree></p:cSld></p:{root}>"#).into_bytes()
    }

    /// Two slides listed in the reverse of their file names, the first with a title, a body
    /// inheriting its geometry from the layout, a grouped picture and notes.
    fn sample() -> Vec<u8> {
        let presentation = format!(
            r#"<p:presentation {NS}><p:sldIdLst><p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/></p:presentation>"#
        );
        let first = tree("sld", concat!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="457200" y="274320"/><a:ext cx="10972800" cy="1143000"/></a:xfrm></p:spPr>"#,
            r#"<p:txBody><a:p><a:r><a:rPr sz="4000" b="1"/><a:t>Quarterly results &amp; plans</a:t></a:r></a:p></p:txBody></p:sp>"#,
            r#"<p:sp><p:nvSpPr><p:cNvPr id="3" name="Content 2"/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/>"#,
            r#"<p:txBody><a:p><a:r><a:t>Revenue up</a:t></a:r></a:p><a:p><a:r><a:t>Costs</a:t></a:r><a:br/><a:r><a:t>down</a:t></a:r></a:p><a:p><a:endParaRPr/></a:p></p:txBody></p:sp>"#,
            r#"<p:grpSp><p:nvGrpSpPr><p:cNvPr id="4" name="Group 3"/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="1270000" y="4445000"/><a:ext cx="2540000" cy="2540000"/><a:chOff x="0" y="0"/><a:chExt cx="1270000" cy="1270000"/></a:xfrm></p:grpSpPr>"#,
            r#"<p:pic><p:nvPicPr><p:cNvPr id="5" name="Picture 4"/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"/></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="0"/><a:ext cx="635000" cy="635000"/></a:xfrm></p:spPr></p:pic></p:grpSp>"#,
        ));
        let second = tree("sld", r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:nvPr><p:ph type="ctrTitle"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="12192000" cy="1270000"/></a:xfrm></p:spPr><p:txBody><a:p><a:r><a:t>Second</a:t></a:r></a:p></p:txBody></p:sp>"#);
        let layout = tree("sldLayout", r#"<p:sp><p:nvSpPr><p:cNvPr id="3" name="Content Placeholder 2"/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="838200" y="1825625"/><a:ext cx="10515600" cy="4351338"/></a:xfrm></p:spPr></p:sp>"#);
        let notes = tree("notes", r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image"/><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes"/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Speaker notes here</a:t></a:r></a:p></p:txBody></p:sp>"#);
        let mut png = Cursor::new(Vec::new());
        ::image::RgbaImage::from_pixel(4, 4, ::image::Rgba([255, 0, 0, 255])).write_to(&mut png, ::image::ImageFormat::Png).unwrap();
        zip_parts(&[
            ("ppt/presentation.xml", presentation.into_bytes()),
            ("ppt/_rels/presentation.xml.rels", rels(&[("rId2", "slide", "slides/slide1.xml"), ("rId3", "slide", "/ppt/slides/slide2.xml")])),
            ("ppt/slides/slide1.xml", second),
            ("ppt/slides/slide2.xml", first),
            (
                "ppt/slides/_rels/slide2.xml.rels",
                rels(&[
                    ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
                    ("rId2", "image", "../media/image1.png"),
                    ("rId3", "notesSlide", "../notesSlides/notesSlide1.xml"),
                ]),
            ),
            ("ppt/slideLayouts/slideLayout1.xml", layout),
            ("ppt/notesSlides/notesSlide1.xml", notes),
            ("ppt/media/image1.png", png.into_inner()),
        ])
    }

    #[test]
    fn a_presentation_round_trips_to_slides_shapes_text_and_pixels() {
        let doc = PptxDoc::parse(&sample()).unwrap();
        assert_eq!((doc.size, doc.page_count()), (PageSize { width: 960.0, height: 540.0 }, 2));
        assert_eq!(
            doc.text(),
            "Slide 1\nQuarterly results & plans\nRevenue up\nCosts\ndown\nNotes: Speaker notes here\n\nSlide 2\nSecond\n"
        );
        let shapes = &doc.slides[0].shapes;
        let geometry: Vec<_> = shapes
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.x.round(), s.y.round(), s.width.round(), s.height.round()))
            .collect();
        assert_eq!(
            geometry,
            [
                ("Title 1", ShapeKind::Text, 36.0, 22.0, 864.0, 90.0),
                ("Content 2", ShapeKind::Text, 66.0, 144.0, 828.0, 343.0),
                ("Picture 4", ShapeKind::Picture, 120.0, 350.0, 100.0, 100.0),
            ]
        );
        assert_eq!(shapes[0].styles[0], ParaStyle { size: Some(40.0), bold: true });
        assert_eq!(shapes[2].image.as_deref(), Some("ppt/media/image1.png"));

        let page = doc.render_page(0, 0.5, &Fonts::none()).unwrap();
        assert_eq!((page.width, page.height), (480, 270));
        assert_eq!(page.pixel(85, 200), Some([255, 0, 0, 255]));
        assert_eq!(page.pixel(50, 200), Some([255, 255, 255, 255]));
        let dark = |y0: u32, y1| (y0..y1).any(|y| (0..480).any(|x| page.pixel(x, y).is_some_and(|p| p[0] < 128)));
        assert!(dark(10, 56) && dark(72, 160) && !dark(57, 70));
        assert!(matches!(doc.render_page(2, 1.0, &Fonts::none()), Err(DocsError::PageRange { index: 2, count: 2 })));
        assert!(matches!(PptxDoc::parse(&zip_parts(&[])), Err(DocsError::Invalid { format: Format::Pptx, .. })));
    }
}
