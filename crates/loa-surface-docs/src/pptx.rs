//! PPTX presentations: our own reader over the package zip and the slide XML,
//! slide text and shape geometry for the agent, and a simple slide renderer.

use crate::raster::{Canvas, Color, Fonts, TextStyle};
use crate::{DocsError, Format, PageSize, Paged, Result, Rgba};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::QName;
use quick_xml::{Reader, XmlVersion};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Cursor, Read};

/// English Metric Units per point.
const EMU_PER_POINT: f64 = 12700.0;
/// Largest decompressed part accepted, a guard against zip bombs.
const MAX_PART_BYTES: u64 = 512 << 20;
/// Font size of runs without `sz`, in points.
const DEFAULT_FONT_SIZE: f32 = 18.0;
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

/// A parsed presentation: slides in presentation order and the bytes of the
/// pictures they show.
#[derive(Clone, Debug)]
pub struct PptxDoc {
    size: PageSize,
    slides: Vec<Slide>,
    /// Picture bytes by part path (`ppt/media/image1.png`).
    media: HashMap<String, Vec<u8>>,
}

/// One slide's content.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Slide {
    /// Position in presentation order, from 0 (the page index).
    pub index: usize,
    /// Text of the first title or centered title placeholder.
    pub title: Option<String>,
    /// Non empty paragraphs (table rows for tables) of the other text shapes,
    /// in document order.
    pub body: Vec<String>,
    /// Text of the notes slide's body placeholder.
    pub notes: Option<String>,
    pub shapes: Vec<Shape>,
}

/// What a shape on a slide is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShapeKind {
    Text,
    Picture,
    Table,
    Other,
}

/// A shape on a slide. Geometry is in points from the slide's top left
/// corner, after group transforms; placeholders without their own geometry
/// take it from the slide layout or master.
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
    /// One entry per paragraph, line breaks as `\n`. Tables give one entry
    /// per row, cells separated by ` | `.
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
    /// Parses a PPTX file.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(invalid)?;
        let mut pkg = Package { zip };
        let presentation = pkg
            .xml(PRESENTATION_PART)?
            .ok_or_else(|| invalid(format!("{PRESENTATION_PART} is missing")))?;
        let (size, order) = parse_presentation(&presentation)
            .map_err(|m| invalid(format!("{PRESENTATION_PART}: {m}")))?;
        let presentation_rels = pkg.rels(PRESENTATION_PART)?;

        let mut templates = HashMap::new();
        let mut media = HashMap::new();
        let mut slides = Vec::with_capacity(order.len());
        for (index, rid) in order.iter().enumerate() {
            let part = presentation_rels
                .iter()
                .find(|r| r.id == *rid && r.kind.ends_with(REL_SLIDE))
                .map(|r| r.target.as_str())
                .ok_or_else(|| invalid(format!("slide relationship {rid} is missing")))?;
            let slide = read_slide(&mut pkg, &mut templates, &mut media, index, part)?;
            slides.push(slide);
        }
        Ok(Self { size: size.unwrap_or(DEFAULT_SLIDE_SIZE), slides, media })
    }

    /// Slides in presentation order.
    pub fn slides(&self) -> &[Slide] {
        &self.slides
    }

    /// Size of every slide, in points.
    pub fn slide_size(&self) -> PageSize {
        self.size
    }

    /// Plain text of the presentation: per slide a `Slide N` header, the
    /// title, the body paragraphs and the notes.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for slide in &self.slides {
            if !out.is_empty() {
                out.push('\n');
            }
            // Writing to a String cannot fail.
            let _ = writeln!(out, "Slide {}", slide.index + 1);
            if let Some(title) = &slide.title {
                out.push_str(title);
                out.push('\n');
            }
            for paragraph in &slide.body {
                out.push_str(paragraph);
                out.push('\n');
            }
            if let Some(notes) = &slide.notes {
                out.push_str("Notes: ");
                out.push_str(notes);
                out.push('\n');
            }
        }
        out
    }

    fn slide(&self, index: usize) -> Result<&Slide> {
        self.slides.get(index).ok_or_else(|| DocsError::page_range(index, self.slides.len()))
    }

    /// Draws a picture into its box, or a gray box when it can't be decoded.
    fn draw_picture(&self, canvas: &mut Canvas, path: Option<&str>, x: f32, y: f32, w: f32, h: f32) {
        let decoded = path
            .and_then(|p| self.media.get(p))
            .and_then(|bytes| ::image::load_from_memory(bytes).ok())
            .map(|img| {
                let img = img.into_rgba8();
                Rgba { width: img.width(), height: img.height(), data: img.into_raw() }
            });
        match decoded {
            Some(img) => canvas.blit(&img, x, y, w, h),
            None => canvas.fill_rect(x, y, w, h, Color::GRAY),
        }
    }
}

impl Paged for PptxDoc {
    fn page_count(&self) -> usize {
        self.slides.len()
    }

    fn page_size(&self, index: usize) -> Result<PageSize> {
        self.slide(index)?;
        Ok(self.size)
    }

    fn render_page(&self, index: usize, scale: f32, fonts: &Fonts) -> Result<Rgba> {
        let slide = self.slide(index)?;
        let (width, height) = self.size.pixels(scale);
        let mut canvas = Canvas::new(width, height, Color::WHITE);
        for shape in &slide.shapes {
            let is_title = is_title(shape.placeholder.as_deref());
            let Some((x, y, w, h)) = shape_box(shape, is_title, self.size) else { continue };
            let (x, y, w, h) = (x * scale, y * scale, w * scale, h * scale);
            match shape.kind {
                ShapeKind::Picture => {
                    self.draw_picture(&mut canvas, shape.image.as_deref(), x, y, w, h);
                }
                ShapeKind::Table => canvas.stroke_rect(x, y, w, h, scale, Color::LIGHT_GRAY),
                ShapeKind::Text | ShapeKind::Other => {}
            }
            draw_paragraphs(&mut canvas, fonts, shape, is_title, x, y, w, scale);
        }
        Ok(canvas.into_rgba())
    }
}

fn invalid(message: impl std::fmt::Display) -> DocsError {
    DocsError::invalid(Format::Pptx, message)
}

fn is_title(placeholder: Option<&str>) -> bool {
    matches!(placeholder, Some("title" | "ctrTitle"))
}

/// The box a shape is drawn in, in points. Placeholders whose geometry
/// couldn't be resolved get the usual title or body area.
fn shape_box(shape: &Shape, is_title: bool, slide: PageSize) -> Option<(f32, f32, f32, f32)> {
    if shape.width > 0.0 && shape.height > 0.0 {
        return Some((shape.x, shape.y, shape.width, shape.height));
    }
    if shape.placeholder.is_none() || shape.paragraphs.is_empty() {
        return None;
    }
    let (w, h) = (slide.width, slide.height);
    Some(if is_title {
        (w * 0.05, h * 0.04, w * 0.9, h * 0.16)
    } else {
        (w * 0.05, h * 0.24, w * 0.9, h * 0.7)
    })
}

/// Draws a shape's paragraphs wrapped to its box, top aligned.
#[allow(clippy::too_many_arguments)]
fn draw_paragraphs(
    canvas: &mut Canvas,
    fonts: &Fonts,
    shape: &Shape,
    is_title: bool,
    x: f32,
    y: f32,
    w: f32,
    scale: f32,
) {
    let left = x + INSET_X * scale;
    let max_width = (w - 2.0 * INSET_X * scale).max(1.0);
    let bottom = canvas.height() as f32;
    let mut top = y + INSET_Y * scale;
    for (i, paragraph) in shape.paragraphs.iter().enumerate() {
        let style = shape.styles.get(i).copied().unwrap_or_default();
        let size = style.size.unwrap_or(DEFAULT_FONT_SIZE) * scale;
        let bold = style.bold || is_title;
        let text_style = TextStyle::new(size).bold(bold);
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
    let xml = pkg.xml(part)?.ok_or_else(|| invalid(format!("slide part {part} is missing")))?;
    let parsed = parse_shapes(&xml).map_err(|m| invalid(format!("{part}: {m}")))?;
    let rels = pkg.rels(part)?;
    let layout = rels.iter().find(|r| r.kind.ends_with(REL_LAYOUT)).map(|r| r.target.as_str());
    if let Some(layout) = layout {
        load_template(pkg, templates, layout)?;
    }

    let mut shapes = Vec::with_capacity(parsed.len());
    for p in parsed {
        let rect = p.rect.or_else(|| {
            let kind = p.placeholder.as_deref()?;
            inherited_rect(templates, layout, kind, p.ph_idx.as_deref())
        });
        let image = p
            .embed
            .as_deref()
            .and_then(|id| rels.iter().find(|r| r.id == id))
            .map(|r| r.target.clone());
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

    let title = shapes
        .iter()
        .find(|s| is_title(s.placeholder.as_deref()))
        .and_then(|s| joined_text(&s.paragraphs));
    let mut seen_title = false;
    let mut body = Vec::new();
    for shape in &shapes {
        if is_title(shape.placeholder.as_deref()) && !seen_title {
            seen_title = true;
            continue;
        }
        if !matches!(shape.kind, ShapeKind::Text | ShapeKind::Table)
            || shape.placeholder.as_deref() == Some("sldNum")
        {
            continue;
        }
        body.extend(shape.paragraphs.iter().filter(|p| !p.trim().is_empty()).cloned());
    }

    let notes = match rels.iter().find(|r| r.kind.ends_with(REL_NOTES)) {
        Some(rel) => read_notes(pkg, &rel.target)?,
        None => None,
    };
    Ok(Slide { index, title, body, notes, shapes })
}

/// Text of a notes slide's body placeholder.
fn read_notes(pkg: &mut Package<'_>, part: &str) -> Result<Option<String>> {
    let Some(xml) = pkg.xml(part)? else { return Ok(None) };
    let parsed = parse_shapes(&xml).map_err(|m| invalid(format!("{part}: {m}")))?;
    let paragraphs: Vec<&str> = parsed
        .iter()
        .filter(|s| s.placeholder.as_deref() == Some("body"))
        .flat_map(|s| s.paragraphs.iter().map(String::as_str))
        .filter(|p| !p.trim().is_empty())
        .collect();
    Ok((!paragraphs.is_empty()).then(|| paragraphs.join("\n")))
}

/// Non empty paragraphs joined with newlines, `None` when there are none.
fn joined_text(paragraphs: &[String]) -> Option<String> {
    let mut out = String::new();
    for p in paragraphs.iter().filter(|p| !p.trim().is_empty()) {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(p);
    }
    (!out.is_empty()).then_some(out)
}

/// Placeholder geometry of a slide layout or master.
struct Template {
    placeholders: Vec<TemplatePlaceholder>,
    /// The master of a layout.
    parent: Option<String>,
}

struct TemplatePlaceholder {
    kind: String,
    idx: Option<String>,
    rect: Option<Rect>,
}

/// Parses a layout or master part (and a layout's master) into `templates`.
fn load_template(
    pkg: &mut Package<'_>,
    templates: &mut HashMap<String, Template>,
    part: &str,
) -> Result<()> {
    if templates.contains_key(part) {
        return Ok(());
    }
    let Some(xml) = pkg.xml(part)? else {
        templates.insert(part.to_owned(), Template { placeholders: Vec::new(), parent: None });
        return Ok(());
    };
    let placeholders = parse_shapes(&xml)
        .map_err(|m| invalid(format!("{part}: {m}")))?
        .into_iter()
        .filter_map(|s| Some(TemplatePlaceholder { kind: s.placeholder?, idx: s.ph_idx, rect: s.rect }))
        .collect();
    let parent = pkg
        .rels(part)?
        .into_iter()
        .find(|r| r.kind.ends_with(REL_MASTER))
        .map(|r| r.target);
    // Inserted before the parent is loaded so a cycle of relationships ends.
    templates.insert(part.to_owned(), Template { placeholders, parent: parent.clone() });
    if let Some(parent) = parent {
        load_template(pkg, templates, &parent)?;
    }
    Ok(())
}

/// Placeholder types that inherit from the same master placeholder.
fn placeholder_family(kind: &str) -> &str {
    match kind {
        "ctrTitle" => "title",
        "subTitle" | "obj" => "body",
        other => other,
    }
}

/// Geometry of a placeholder from the layout (matched by index, then type)
/// or its master (matched by type).
fn inherited_rect(
    templates: &HashMap<String, Template>,
    layout: Option<&str>,
    kind: &str,
    idx: Option<&str>,
) -> Option<Rect> {
    let family = placeholder_family(kind);
    let mut part = layout;
    let mut first = true;
    while let Some(name) = part {
        let template = templates.get(name)?;
        let by_idx = || {
            first
                .then_some(idx)
                .flatten()
                .and_then(|idx| template.placeholders.iter().find(|p| p.idx.as_deref() == Some(idx)))
        };
        let found = by_idx().or_else(|| {
            template.placeholders.iter().find(|p| placeholder_family(&p.kind) == family)
        });
        if let Some(rect) = found.and_then(|p| p.rect) {
            return Some(rect);
        }
        part = template.parent.as_deref();
        first = false;
    }
    None
}

/// The package zip.
struct Package<'b> {
    zip: zip::ZipArchive<Cursor<&'b [u8]>>,
}

/// A resolved package relationship.
struct Rel {
    id: String,
    kind: String,
    /// Part path inside the zip, without a leading `/`.
    target: String,
}

impl Package<'_> {
    /// Bytes of a part, `None` when the zip has no such entry.
    fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>> {
        let file = match self.zip.by_name(name) {
            Ok(file) => file,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(invalid(format!("{name}: {e}"))),
        };
        let mut out = Vec::new();
        file.take(MAX_PART_BYTES + 1)
            .read_to_end(&mut out)
            .map_err(|e| invalid(format!("{name}: {e}")))?;
        if out.len() as u64 > MAX_PART_BYTES {
            return Err(invalid(format!("{name} is larger than {} MiB", MAX_PART_BYTES >> 20)));
        }
        Ok(Some(out))
    }

    /// An XML part as UTF-8 text.
    fn xml(&mut self, name: &str) -> Result<Option<String>> {
        let Some(bytes) = self.read(name)? else { return Ok(None) };
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| invalid(format!("{name} is not UTF-8 XML")))
    }

    /// The relationships of `part`, none when it has no rels part.
    fn rels(&mut self, part: &str) -> Result<Vec<Rel>> {
        let path = rels_path(part);
        let Some(xml) = self.xml(&path)? else { return Ok(Vec::new()) };
        parse_rels(&xml, part).map_err(|m| invalid(format!("{path}: {m}")))
    }
}

/// `ppt/slides/slide1.xml` -> `ppt/slides/_rels/slide1.xml.rels`.
fn rels_path(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, name)) => format!("{dir}/_rels/{name}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

/// Resolves a relationship target against the directory of its source part.
fn resolve_target(source: &str, target: &str) -> String {
    if let Some(absolute) = target.strip_prefix('/') {
        return absolute.to_owned();
    }
    let mut segments: Vec<&str> = match source.rsplit_once('/') {
        Some((dir, _)) => dir.split('/').collect(),
        None => Vec::new(),
    };
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            s => segments.push(s),
        }
    }
    segments.join("/")
}

/// Strips a byte order mark before parsing.
fn xml_reader(xml: &str) -> Reader<&[u8]> {
    Reader::from_str(xml.strip_prefix('\u{feff}').unwrap_or(xml))
}

/// Value of the first attribute whose name matches `matches`, entities
/// resolved.
fn find_attr<'e>(
    e: &'e BytesStart<'_>,
    matches: impl Fn(QName<'_>) -> bool,
) -> std::result::Result<Option<Cow<'e, str>>, String> {
    for attr in e.attributes().with_checks(false) {
        let attr = attr.map_err(|err| err.to_string())?;
        if matches(attr.key) {
            return attr.normalized_value(XmlVersion::Implicit1_0).map(Some).map_err(|err| err.to_string());
        }
    }
    Ok(None)
}

/// Attribute by local name, ignoring any prefix.
fn attr<'e>(e: &'e BytesStart<'_>, local: &str) -> std::result::Result<Option<Cow<'e, str>>, String> {
    find_attr(e, |key| key.local_name().as_ref() == local)
}

/// A numeric attribute (EMU, hundredths of a point).
fn num_attr(e: &BytesStart<'_>, local: &str) -> std::result::Result<Option<f64>, String> {
    match attr(e, local)? {
        Some(v) => v
            .trim()
            .parse::<i64>()
            .map(|n| Some(n as f64))
            .map_err(|_| format!("attribute {local}=\"{v}\" is not a number")),
        None => Ok(None),
    }
}

/// Slide size and the slide relationship ids in presentation order.
fn parse_presentation(xml: &str) -> std::result::Result<(Option<PageSize>, Vec<String>), String> {
    let mut reader = xml_reader(xml);
    let mut size = None;
    let mut order = Vec::new();
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                "sldSz" => {
                    let (Some(cx), Some(cy)) = (num_attr(&e, "cx")?, num_attr(&e, "cy")?) else {
                        return Err("p:sldSz lacks cx or cy".into());
                    };
                    if cx <= 0.0 || cy <= 0.0 {
                        return Err(format!("slide size {cx} x {cy} EMU is not positive"));
                    }
                    size = Some(PageSize {
                        width: (cx / EMU_PER_POINT) as f32,
                        height: (cy / EMU_PER_POINT) as f32,
                    });
                }
                "sldId" => {
                    // The relationship id is the prefixed `r:id`; the bare `id` is a number.
                    let rid = find_attr(&e, |key| {
                        key.prefix().is_some() && key.local_name().as_ref() == "id"
                    })?
                    .ok_or("p:sldId lacks r:id")?;
                    order.push(rid.into_owned());
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((size, order))
}

/// Internal relationships of `source`, targets resolved to part paths.
fn parse_rels(xml: &str, source: &str) -> std::result::Result<Vec<Rel>, String> {
    let mut reader = xml_reader(xml);
    let mut rels = Vec::new();
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "Relationship" => {
                if attr(&e, "TargetMode")?.is_some_and(|m| m == "External") {
                    continue;
                }
                let (Some(id), Some(kind), Some(target)) =
                    (attr(&e, "Id")?, attr(&e, "Type")?, attr(&e, "Target")?)
                else {
                    return Err("Relationship lacks Id, Type or Target".into());
                };
                rels.push(Rel {
                    id: id.into_owned(),
                    kind: kind.into_owned(),
                    target: resolve_target(source, &target),
                });
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(rels)
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
        Rect {
            x: self.off.x + (r.x - self.child.x) * sx,
            y: self.off.y + (r.y - self.child.y) * sy,
            w: r.w * sx,
            h: r.h * sy,
        }
    }
}

/// A shape as read from a slide, layout, master or notes part.
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

/// The element a shape was opened from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Element {
    Sp,
    Pic,
    GraphicFrame,
    Connector,
}

/// A shape being read.
struct Builder {
    element: Element,
    name: String,
    placeholder: Option<String>,
    ph_idx: Option<String>,
    rect: Option<Rect>,
    paragraphs: Vec<String>,
    styles: Vec<ParaStyle>,
    embed: Option<String>,
    is_table: bool,
    in_paragraph: bool,
    paragraph: String,
    style: ParaStyle,
    style_from_run: bool,
    cell: Vec<String>,
    row: Vec<String>,
    row_style: Option<ParaStyle>,
}

impl Builder {
    fn new(element: Element) -> Self {
        Self {
            element,
            name: String::new(),
            placeholder: None,
            ph_idx: None,
            rect: None,
            paragraphs: Vec::new(),
            styles: Vec::new(),
            embed: None,
            is_table: false,
            in_paragraph: false,
            paragraph: String::new(),
            style: ParaStyle::default(),
            style_from_run: false,
            cell: Vec::new(),
            row: Vec::new(),
            row_style: None,
        }
    }

    fn finish(self, frames: &[Frame]) -> ParsedShape {
        let rect = self.rect.map(|r| frames.iter().rev().fold(r, |r, f| f.apply(r)));
        let kind = match self.element {
            Element::Sp
                if self.placeholder.is_some()
                    || self.paragraphs.iter().any(|p| !p.trim().is_empty()) =>
            {
                ShapeKind::Text
            }
            Element::Pic => ShapeKind::Picture,
            Element::GraphicFrame if self.is_table => ShapeKind::Table,
            Element::Sp | Element::GraphicFrame | Element::Connector => ShapeKind::Other,
        };
        ParsedShape {
            name: self.name,
            kind,
            placeholder: self.placeholder,
            ph_idx: self.ph_idx,
            rect,
            paragraphs: self.paragraphs,
            styles: self.styles,
            embed: self.embed,
        }
    }
}

/// Reads the shapes of a part's shape tree in document order, groups
/// flattened.
fn parse_shapes(xml: &str) -> std::result::Result<Vec<ParsedShape>, String> {
    let mut reader = xml_reader(xml);
    let mut parser = ShapeParser { stack: Vec::new(), frames: Vec::new(), current: None, shapes: Vec::new() };
    let mut skip = 0usize;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => {
                if skip > 0 {
                    skip += 1;
                    continue;
                }
                match parser.open(&e)? {
                    Tag::Skip => skip = 1,
                    tag => parser.stack.push(tag),
                }
            }
            Event::Empty(e) => {
                if skip == 0 {
                    let tag = parser.open(&e)?;
                    parser.close(tag);
                }
            }
            Event::End(_) => {
                if skip > 0 {
                    skip -= 1;
                } else if let Some(tag) = parser.stack.pop() {
                    parser.close(tag);
                }
            }
            Event::Text(t) if skip == 0 => {
                if let Some(text) = parser.text_target() {
                    text.push_str(&t.xml10_content());
                }
            }
            Event::CData(t) if skip == 0 => {
                if let Some(text) = parser.text_target() {
                    text.push_str(&t);
                }
            }
            Event::GeneralRef(r) if skip == 0 => {
                if let Some(text) = parser.text_target() {
                    if let Some(c) = r.resolve_char_ref().map_err(|e| e.to_string())? {
                        text.push(c);
                    } else {
                        let name: &str = &r;
                        let value = quick_xml::escape::resolve_predefined_entity(name)
                            .ok_or_else(|| format!("unknown entity &{name};"))?;
                        text.push_str(value);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(parser.shapes)
}

struct ShapeParser {
    stack: Vec<Tag>,
    /// Transforms of the open groups, the shape tree first.
    frames: Vec<Frame>,
    current: Option<Builder>,
    shapes: Vec<ParsedShape>,
}

impl ShapeParser {
    /// The paragraph text receives character data when inside `a:t`.
    fn text_target(&mut self) -> Option<&mut String> {
        if self.stack.last() != Some(&Tag::Text) {
            return None;
        }
        self.current.as_mut().filter(|b| b.in_paragraph).map(|b| &mut b.paragraph)
    }

    /// Handles an opening tag and returns what to track until it closes.
    fn open(&mut self, e: &BytesStart<'_>) -> std::result::Result<Tag, String> {
        let local = e.local_name();
        let in_xfrm = self.stack.last() == Some(&Tag::Xfrm);
        let tag = match local.as_ref() {
            "spTree" | "grpSp" if self.current.is_none() => {
                self.frames.push(Frame::default());
                Tag::Group
            }
            name @ ("sp" | "pic" | "graphicFrame" | "cxnSp") if self.current.is_none() => {
                let element = match name {
                    "sp" => Element::Sp,
                    "pic" => Element::Pic,
                    "graphicFrame" => Element::GraphicFrame,
                    _ => Element::Connector,
                };
                self.current = Some(Builder::new(element));
                Tag::Shape
            }
            "Fallback" => Tag::Skip,
            "xfrm" => Tag::Xfrm,
            name @ ("off" | "ext" | "chOff" | "chExt") if in_xfrm => {
                let (kx, ky) = if name.ends_with("ff") { ("x", "y") } else { ("cx", "cy") };
                let a = num_attr(e, kx)?.unwrap_or(0.0);
                let b = num_attr(e, ky)?.unwrap_or(0.0);
                let target = match &mut self.current {
                    Some(builder) => match name {
                        "off" | "ext" => Some(builder.rect.get_or_insert_with(Rect::default)),
                        _ => None,
                    },
                    None => self.frames.last_mut().map(|f| match name {
                        "off" | "ext" => &mut f.off,
                        _ => &mut f.child,
                    }),
                };
                if let Some(rect) = target {
                    if name.ends_with("ff") {
                        (rect.x, rect.y) = (a, b);
                    } else {
                        (rect.w, rect.h) = (a, b);
                    }
                }
                Tag::Other
            }
            "cNvPr" => {
                if let Some(builder) = self.current.as_mut().filter(|b| b.name.is_empty())
                    && let Some(name) = attr(e, "name")?
                {
                    builder.name = name.into_owned();
                }
                Tag::Other
            }
            "ph" => {
                if let Some(builder) = self.current.as_mut() {
                    builder.placeholder =
                        Some(attr(e, "type")?.map_or_else(|| "obj".to_owned(), Cow::into_owned));
                    builder.ph_idx = attr(e, "idx")?.map(Cow::into_owned);
                }
                Tag::Other
            }
            "blip" => {
                if let Some(builder) = self.current.as_mut()
                    && builder.embed.is_none()
                {
                    builder.embed = attr(e, "embed")?.map(Cow::into_owned);
                }
                Tag::Other
            }
            "tbl" => {
                if let Some(builder) = self.current.as_mut() {
                    builder.is_table = true;
                }
                Tag::Other
            }
            "tr" => Tag::TableRow,
            "tc" => {
                if let Some(builder) = self.current.as_mut() {
                    builder.cell.clear();
                }
                Tag::TableCell
            }
            "p" => match self.current.as_mut() {
                Some(builder) => {
                    builder.in_paragraph = true;
                    builder.paragraph.clear();
                    builder.style = ParaStyle::default();
                    builder.style_from_run = false;
                    Tag::Paragraph
                }
                None => Tag::Other,
            },
            "t" => Tag::Text,
            "br" => {
                if let Some(builder) = self.current.as_mut().filter(|b| b.in_paragraph) {
                    builder.paragraph.push('\n');
                }
                Tag::Other
            }
            name @ ("rPr" | "endParaRPr") => {
                if let Some(builder) = self.current.as_mut().filter(|b| b.in_paragraph && !b.style_from_run) {
                    builder.style = ParaStyle {
                        size: num_attr(e, "sz")?.map(|sz| (sz / 100.0) as f32).filter(|s| *s > 0.0),
                        bold: attr(e, "b")?.is_some_and(|b| b == "1" || b == "true"),
                    };
                    builder.style_from_run = name == "rPr";
                }
                Tag::Other
            }
            _ => Tag::Other,
        };
        Ok(tag)
    }

    /// Handles the end of an element opened as `tag`.
    fn close(&mut self, tag: Tag) {
        match tag {
            Tag::Group => {
                self.frames.pop();
            }
            Tag::Shape => {
                if let Some(builder) = self.current.take() {
                    self.shapes.push(builder.finish(&self.frames));
                }
            }
            Tag::Paragraph => {
                let in_cell = self.stack.contains(&Tag::TableCell);
                if let Some(builder) = self.current.as_mut() {
                    builder.in_paragraph = false;
                    let text = std::mem::take(&mut builder.paragraph);
                    if in_cell {
                        builder.row_style.get_or_insert(builder.style);
                        if !text.trim().is_empty() {
                            builder.cell.push(text);
                        }
                    } else {
                        builder.paragraphs.push(text);
                        builder.styles.push(builder.style);
                    }
                }
            }
            Tag::TableCell => {
                if let Some(builder) = self.current.as_mut() {
                    let cell = builder.cell.join(" ");
                    builder.row.push(cell);
                    builder.cell.clear();
                }
            }
            Tag::TableRow => {
                if let Some(builder) = self.current.as_mut() {
                    builder.paragraphs.push(builder.row.join(" | "));
                    builder.styles.push(builder.row_style.take().unwrap_or_default());
                    builder.row.clear();
                }
            }
            Tag::Xfrm | Tag::Text | Tag::Skip | Tag::Other => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
    const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const RELS_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

    fn zip_parts(parts: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in parts {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn rels(entries: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut xml = format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{RELS_NS}">"#);
        for (id, kind, target) in entries {
            xml.push_str(&format!(
                r#"<Relationship Id="{id}" Type="{REL_NS}/{kind}" Target="{target}"/>"#
            ));
        }
        xml.push_str("</Relationships>");
        xml.into_bytes()
    }

    fn slide_xml(shapes: &str) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><p:sld {NS}><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr>{shapes}</p:spTree></p:cSld></p:sld>"#
        )
        .into_bytes()
    }

    fn red_png() -> Vec<u8> {
        let img = ::image::RgbaImage::from_pixel(4, 4, ::image::Rgba([255, 0, 0, 255]));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, ::image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// Two slides listed in the reverse of their file names, the first with
    /// a title, a body inheriting its geometry from the layout, a grouped
    /// picture and notes.
    fn sample() -> Vec<u8> {
        let presentation = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><p:presentation {NS}><p:sldIdLst><p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/><p:notesSz cx="6858000" cy="9144000"/></p:presentation>"#
        );
        let first = slide_xml(concat!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>"#,
            r#"<p:spPr><a:xfrm><a:off x="457200" y="274320"/><a:ext cx="10972800" cy="1143000"/></a:xfrm></p:spPr>"#,
            r#"<p:txBody><a:bodyPr/><a:p><a:r><a:rPr lang="en-US" sz="4000" b="1"/><a:t>Quarterly results &amp; plans</a:t></a:r></a:p></p:txBody></p:sp>"#,
            r#"<p:sp><p:nvSpPr><p:cNvPr id="3" name="Content 2"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/>"#,
            r#"<p:txBody><a:bodyPr/><a:p><a:r><a:t>Revenue up</a:t></a:r></a:p><a:p><a:r><a:t>Costs</a:t></a:r><a:br/><a:r><a:t>down</a:t></a:r></a:p><a:p><a:endParaRPr/></a:p></p:txBody></p:sp>"#,
            r#"<p:grpSp><p:nvGrpSpPr><p:cNvPr id="4" name="Group 3"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>"#,
            r#"<p:grpSpPr><a:xfrm><a:off x="1270000" y="4445000"/><a:ext cx="2540000" cy="2540000"/><a:chOff x="0" y="0"/><a:chExt cx="1270000" cy="1270000"/></a:xfrm></p:grpSpPr>"#,
            r#"<p:pic><p:nvPicPr><p:cNvPr id="5" name="Picture 4"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"/><a:stretch><a:fillRect/></a:stretch></p:blipFill>"#,
            r#"<p:spPr><a:xfrm><a:off x="127000" y="0"/><a:ext cx="635000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:pic></p:grpSp>"#,
        ));
        let second = slide_xml(concat!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr/><p:nvPr><p:ph type="ctrTitle"/></p:nvPr></p:nvSpPr>"#,
            r#"<p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="12192000" cy="1270000"/></a:xfrm></p:spPr>"#,
            r#"<p:txBody><a:bodyPr/><a:p><a:r><a:t>Second</a:t></a:r></a:p></p:txBody></p:sp>"#,
        ));
        let layout = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><p:sldLayout {NS}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="3" name="Content Placeholder 2"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="838200" y="1825625"/><a:ext cx="10515600" cy="4351338"/></a:xfrm></p:spPr></p:sp></p:spTree></p:cSld></p:sldLayout>"#
        );
        let notes = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><p:notes {NS}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image"/><p:cNvSpPr/><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr><p:spPr/></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>Speaker notes here</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#
        );
        zip_parts(&[
            ("[Content_Types].xml", br#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/></Types>"#.to_vec()),
            ("ppt/presentation.xml", presentation.into_bytes()),
            (
                "ppt/_rels/presentation.xml.rels",
                rels(&[("rId2", "slide", "slides/slide1.xml"), ("rId3", "slide", "/ppt/slides/slide2.xml")]),
            ),
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
            ("ppt/slideLayouts/slideLayout1.xml", layout.into_bytes()),
            ("ppt/notesSlides/notesSlide1.xml", notes.into_bytes()),
            ("ppt/media/image1.png", red_png()),
        ])
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn slides_follow_the_presentation_order_with_their_text_and_notes() {
        let doc = PptxDoc::parse(&sample()).unwrap();
        assert_eq!(doc.slide_size(), PageSize { width: 960.0, height: 540.0 });
        assert_eq!(doc.page_count(), 2);
        let slides = doc.slides();
        assert_eq!(slides[0].index, 0);
        assert_eq!(slides[0].title.as_deref(), Some("Quarterly results & plans"));
        assert_eq!(slides[0].body, ["Revenue up", "Costs\ndown"]);
        assert_eq!(slides[0].notes.as_deref(), Some("Speaker notes here"));
        assert_eq!(slides[1].title.as_deref(), Some("Second"));
        assert!(slides[1].body.is_empty());
        assert_eq!(slides[1].notes, None);
        assert_eq!(
            doc.text(),
            "Slide 1\nQuarterly results & plans\nRevenue up\nCosts\ndown\nNotes: Speaker notes here\n\nSlide 2\nSecond\n"
        );
    }

    #[test]
    fn shapes_carry_kind_geometry_and_resolved_picture() {
        let doc = PptxDoc::parse(&sample()).unwrap();
        let shapes = &doc.slides()[0].shapes;
        let names: Vec<&str> = shapes.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Title 1", "Content 2", "Picture 4"]);

        let title = &shapes[0];
        assert_eq!(title.kind, ShapeKind::Text);
        assert_eq!(title.placeholder.as_deref(), Some("title"));
        assert!(close(title.x, 36.0) && close(title.y, 21.6));
        assert!(close(title.width, 864.0) && close(title.height, 90.0));
        assert_eq!(title.styles[0], ParaStyle { size: Some(40.0), bold: true });

        let body = &shapes[1];
        assert_eq!(body.placeholder.as_deref(), Some("body"));
        assert_eq!(body.paragraphs, ["Revenue up", "Costs\ndown", ""]);
        assert!(close(body.x, 66.0) && close(body.y, 143.75), "{body:?}");
        assert!(close(body.width, 828.0) && close(body.height, 342.62), "{body:?}");

        let picture = &shapes[2];
        assert_eq!(picture.kind, ShapeKind::Picture);
        assert_eq!(picture.image.as_deref(), Some("ppt/media/image1.png"));
        assert!(close(picture.x, 120.0) && close(picture.y, 350.0), "{picture:?}");
        assert!(close(picture.width, 100.0) && close(picture.height, 100.0), "{picture:?}");
    }

    #[test]
    fn render_draws_picture_and_text_in_their_boxes() {
        let doc = PptxDoc::parse(&sample()).unwrap();
        let fonts = Fonts::none();
        let page = doc.render_page(0, 0.5, &fonts).unwrap();
        assert_eq!((page.width, page.height), (480, 270));
        // Picture box: 60..110 x 175..225 at half scale.
        assert_eq!(page.pixel(85, 200), Some([255, 0, 0, 255]));
        assert_eq!(page.pixel(50, 200), Some([255, 255, 255, 255]));

        let dark_in = |x0: u32, y0: u32, x1: u32, y1: u32| {
            (y0..y1)
                .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                .filter(|&(x, y)| page.pixel(x, y).is_some_and(|p| p[0] < 128 && p[1] < 128))
                .count()
        };
        // Title box: 18..450 x 10.8..55.8 at half scale.
        assert!(dark_in(18, 10, 450, 56) > 0);
        // Body box from the layout: 33..447 x 71.9..243.
        assert!(dark_in(33, 72, 447, 243) > 0);
        // Nothing drawn between the title and the body.
        assert_eq!(dark_in(0, 57, 480, 70), 0);

        assert_eq!(doc.page_size(1).unwrap(), PageSize { width: 960.0, height: 540.0 });
        assert!(matches!(doc.render_page(2, 1.0, &fonts), Err(DocsError::PageRange { index: 2, count: 2 })));
    }

    #[test]
    fn missing_presentation_part_is_invalid() {
        let bytes = zip_parts(&[("[Content_Types].xml", b"<Types/>".to_vec())]);
        assert!(matches!(PptxDoc::parse(&bytes), Err(DocsError::Invalid { format: Format::Pptx, .. })));
        assert!(matches!(
            PptxDoc::parse(b"not a zip"),
            Err(DocsError::Invalid { format: Format::Pptx, .. })
        ));
    }
}
