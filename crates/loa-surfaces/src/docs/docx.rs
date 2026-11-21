//! DOCX documents read with `docx-rs`: an owned outline for the agent
//! (paragraphs with heading level, list numbering and run formatting, and
//! tables) and a simple paginated page renderer.
//!
//! The renderer is a flow layout, not Word's: blocks run top to bottom inside
//! the page margins, headings bigger and bold, list items indented under a
//! marker, tables as equal width gridded cells with wrapped text.

use crate::docs::raster::{Canvas, Color, Fonts, TextStyle};
use crate::docs::{DocsError, Format, PageSize, Paged, Result, Rgba};
use std::collections::HashMap;

/// Page used when the document has no section properties: US Letter.
const LETTER: PageSize = PageSize { width: 612.0, height: 792.0 };
/// Margin on every side when the document has no section properties.
const DEFAULT_MARGIN: f32 = 72.0;
const TWIPS_PER_POINT: f32 = 20.0;
/// Body text size in points.
const BODY_SIZE: f32 = 11.0;
/// Indent per list level in points.
const LIST_INDENT: f32 = 18.0;
/// Padding between a table cell's border and its text, in points.
const CELL_PADDING: f32 = 4.0;
/// Advance of a tab, in ems. Matches the box metric the layout is made with.
const TAB_EM: f32 = 0.6;
/// List levels OOXML defines (`ilvl` 0 to 8).
const LIST_LEVELS: usize = 9;
/// Bound on `basedOn` chains, which a malformed file can make cyclic.
const MAX_STYLE_DEPTH: usize = 16;

/// The document's content in reading order.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Outline {
    pub blocks: Vec<Block>,
}

/// A top level block of the body.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
}

/// A paragraph of the body.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Paragraph {
    /// Paragraph style id (`w:pStyle`), as used in the document.
    pub style: Option<String>,
    /// 0 for a title, 1 to 9 for headings, from the style's name or id.
    pub heading_level: Option<u8>,
    pub list: Option<ListItem>,
    pub runs: Vec<Run>,
    /// The runs' text concatenated; tabs are `\t`, line and page breaks `\n`.
    pub text: String,
    /// The paragraph starts on a new page (`w:pageBreakBefore`).
    pub page_break_before: bool,
    /// The paragraph holds a page break; the layout starts a new page after it.
    pub page_break_after: bool,
}

/// List membership of a paragraph (`w:numPr`).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ListItem {
    /// Numbering instance (`w:numId`).
    pub num_id: usize,
    /// Nesting level (`w:ilvl`), from 0.
    pub level: u8,
    /// The rendered marker, such as `1.`, `a)` or `•`.
    pub marker: String,
}

/// A run of text sharing one formatting.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// A table; each cell is its paragraphs' text joined by `\n`.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Table {
    pub rows: Vec<Vec<String>>,
}

/// Page margins in points.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Margins {
    top: f32,
    right: f32,
    bottom: f32,
    left: f32,
}

/// A parsed DOCX file.
///
/// Pagination is computed once at parse time with the box metrics of
/// [`Fonts::none`], so [`Paged::page_count`] and every render agree whatever
/// fonts the viewer passes. Real fonts draw the same lines at the same
/// positions; their glyphs are narrower than the 0.6 em boxes, so lines still
/// fit the margins. A table row taller than a page is clipped at the page
/// bottom. All pages take the size of the document's final section.
#[derive(Clone, Debug)]
pub struct DocxDoc {
    outline: Outline,
    page: PageSize,
    pages: Vec<Vec<Op>>,
}

/// What a laid out text line draws.
#[derive(Copy, Clone, Debug)]
enum Source {
    /// Line of the paragraph at `block`; `heading` draws every run bold.
    Paragraph { block: usize, heading: bool },
    Cell { block: usize, row: usize, col: usize },
}

/// A drawing operation at scale 1, in points.
#[derive(Copy, Clone, Debug)]
enum Op {
    /// The bytes `start..end` of the source text, top edge at `y`.
    Text { x: f32, y: f32, size: f32, source: Source, start: usize, end: usize },
    /// The list marker of the paragraph at `block`.
    Marker { x: f32, y: f32, size: f32, block: usize },
    Border { x: f32, y: f32, w: f32, h: f32 },
}

impl DocxDoc {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        // docx-rs indexes attribute lists without checking them (`<w:u/>`,
        // `<w:start/>`), so a malformed part panics inside the reader.
        let docx = std::panic::catch_unwind(|| docx_rs::read_docx(bytes))
            .map_err(|_| DocsError::invalid(Format::Docx, "malformed document part"))?
            .map_err(|e| DocsError::invalid(Format::Docx, e))?;

        let mut reader = Reader::new(&docx.styles, &docx.numberings);
        let mut blocks = Vec::new();
        reader.blocks(&docx.document.children, &mut blocks);
        let outline = Outline { blocks };

        let (page, margins) = section_geometry(&docx.document.section_property);
        let pages = layout(&outline, page, margins, &Fonts::none());
        Ok(Self { outline, page, pages })
    }

    pub fn outline(&self) -> &Outline {
        &self.outline
    }

    /// Plain text: paragraphs joined by `\n`, table cells by `\t`, rows by `\n`.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (i, block) in self.outline.blocks.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            match block {
                Block::Paragraph(p) => out.push_str(&p.text),
                Block::Table(t) => push_table_text(t, &mut out),
            }
        }
        out
    }

    /// `(level, text)` of every title (level 0) and heading, in order.
    pub fn headings(&self) -> Vec<(u8, String)> {
        self.outline
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Paragraph(p) => p.heading_level.map(|level| (level, p.text.clone())),
                Block::Table(_) => None,
            })
            .collect()
    }

    fn draw_op(&self, canvas: &mut Canvas, fonts: &Fonts, scale: f32, op: &Op) {
        match *op {
            Op::Border { x, y, w, h } => {
                canvas.stroke_rect(x * scale, y * scale, w * scale, h * scale, scale, Color::GRAY);
            }
            Op::Marker { x, y, size, block } => {
                if let Some(Block::Paragraph(Paragraph { list: Some(item), .. })) =
                    self.outline.blocks.get(block)
                {
                    let style = TextStyle::new(size * scale);
                    draw_segment(canvas, fonts, style, x * scale, y * scale, &item.marker, false);
                }
            }
            Op::Text { x, y, size, source, start, end } => {
                let (x, y) = (x * scale, y * scale);
                let style = TextStyle::new(size * scale);
                match source {
                    Source::Paragraph { block, heading } => {
                        let Some(Block::Paragraph(p)) = self.outline.blocks.get(block) else { return };
                        draw_paragraph_line(canvas, fonts, style, x, y, p, heading, start, end);
                    }
                    Source::Cell { block, row, col } => {
                        let Some(Block::Table(t)) = self.outline.blocks.get(block) else { return };
                        if let Some(line) =
                            t.rows.get(row).and_then(|r| r.get(col)).and_then(|c| c.get(start..end))
                        {
                            draw_segment(canvas, fonts, style, x, y, line, false);
                        }
                    }
                }
            }
        }
    }
}

impl Paged for DocxDoc {
    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn page_size(&self, index: usize) -> Result<PageSize> {
        if index < self.pages.len() {
            Ok(self.page)
        } else {
            Err(DocsError::page_range(index, self.pages.len()))
        }
    }

    fn render_page(&self, index: usize, scale: f32, fonts: &Fonts) -> Result<Rgba> {
        let ops = self.pages.get(index).ok_or_else(|| DocsError::page_range(index, self.pages.len()))?;
        let (width, height) = self.page.pixels(scale);
        let mut canvas = Canvas::new(width, height, Color::WHITE);
        for op in ops {
            self.draw_op(&mut canvas, fonts, scale, op);
        }
        Ok(canvas.into_rgba())
    }
}

/// Draws bytes `start..end` of a paragraph's text, run by run so bold and
/// underline follow the runs.
#[allow(clippy::too_many_arguments)]
fn draw_paragraph_line(
    canvas: &mut Canvas,
    fonts: &Fonts,
    style: TextStyle,
    x: f32,
    y: f32,
    p: &Paragraph,
    heading: bool,
    start: usize,
    end: usize,
) {
    let mut caret = x;
    let mut run_start = 0;
    for run in &p.runs {
        let run_end = run_start + run.text.len();
        let (a, b) = (start.max(run_start), end.min(run_end));
        run_start = run_end;
        if a >= b {
            continue;
        }
        let Some(segment) = p.text.get(a..b) else { continue };
        let style = style.bold(heading || run.bold);
        caret += draw_segment(canvas, fonts, style, caret, y, segment, run.underline);
    }
}

/// Draws one line piece, advancing tabs by [`TAB_EM`]. Returns the advance.
fn draw_segment(
    canvas: &mut Canvas,
    fonts: &Fonts,
    style: TextStyle,
    x: f32,
    y: f32,
    text: &str,
    underline: bool,
) -> f32 {
    let mut advance = 0.0;
    for (i, piece) in text.split('\t').enumerate() {
        if i > 0 {
            advance += style.size * TAB_EM;
        }
        if !piece.is_empty() {
            advance += canvas.draw_text(fonts, style, x + advance, y, piece);
        }
    }
    if underline {
        let thickness = (style.size / 14.0).max(1.0);
        canvas.fill_rect(x, y + style.size * 1.05, advance, thickness, style.color);
    }
    advance
}

fn push_table_text(table: &Table, out: &mut String) {
    for (r, row) in table.rows.iter().enumerate() {
        if r > 0 {
            out.push('\n');
        }
        for (c, cell) in row.iter().enumerate() {
            if c > 0 {
                out.push('\t');
            }
            out.push_str(cell);
        }
    }
}

/// Page size and margins of the body's section properties.
///
/// docx-rs fills a missing `w:sectPr` with its own defaults, so a section
/// property identical to those defaults is taken as absent and replaced by
/// Letter with 1 inch margins.
fn section_geometry(section: &docx_rs::SectionProperty) -> (PageSize, Margins) {
    let default = (
        LETTER,
        Margins { top: DEFAULT_MARGIN, right: DEFAULT_MARGIN, bottom: DEFAULT_MARGIN, left: DEFAULT_MARGIN },
    );
    if *section == docx_rs::SectionProperty::new() {
        return default;
    }
    // `PageSize` keeps its twips private; its serialization is `{w, h, orient}`.
    let Some((w, h)) = serde_json::to_value(&section.page_size)
        .ok()
        .and_then(|v| Some((v.get("w")?.as_u64()?, v.get("h")?.as_u64()?)))
    else {
        return default;
    };
    if w == 0 || h == 0 {
        return default;
    }
    let page = PageSize { width: w as f32 / TWIPS_PER_POINT, height: h as f32 / TWIPS_PER_POINT };
    let m = &section.page_margin;
    // Negative margins let text overlap the header; clamp so the text area
    // keeps at least a third of the page.
    let margin = |twips: i32, extent: f32| (twips.max(0) as f32 / TWIPS_PER_POINT).min(extent / 3.0);
    let margins = Margins {
        top: margin(m.top, page.height),
        right: margin(m.right, page.width),
        bottom: margin(m.bottom, page.height),
        left: margin(m.left, page.width),
    };
    (page, margins)
}

/// The string a docx-rs value serializes to, for types that keep their
/// field private (`Name`, `BasedOn`, `LevelText`).
fn serialized_str<T: serde::Serialize>(value: &T) -> Option<String> {
    match serde_json::to_value(value).ok()? {
        serde_json::Value::String(s) => Some(s),
        _ => None,
    }
}

/// Heading level of a style name (`heading 1`, `Title`) or id (`Heading1`).
fn heading_level_of(label: &str) -> Option<u8> {
    let lower = label.to_ascii_lowercase();
    if lower == "title" {
        return Some(0);
    }
    let level: u8 = lower.strip_prefix("heading")?.trim().parse().ok()?;
    (1..=9).contains(&level).then_some(level)
}

/// Formats a list counter in an OOXML number format (`w:numFmt`).
fn format_number(n: u32, format: &str) -> String {
    match format {
        "lowerLetter" | "upperLetter" if n > 0 => {
            let letter = b'a' + ((n - 1) % 26) as u8;
            let letter = if format == "upperLetter" { letter.to_ascii_uppercase() } else { letter };
            std::iter::repeat_n(letter as char, ((n - 1) / 26 + 1) as usize).collect()
        }
        "lowerRoman" | "upperRoman" if n > 0 => {
            let roman = roman(n);
            if format == "upperRoman" { roman } else { roman.to_ascii_lowercase() }
        }
        "decimalZero" => format!("{n:02}"),
        _ => n.to_string(),
    }
}

fn roman(mut n: u32) -> String {
    const NUMERALS: [(u32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value, numeral) in NUMERALS {
        while n >= value {
            out.push_str(numeral);
            n -= value;
        }
    }
    out
}

/// Builds the outline from the docx-rs tree, resolving styles and list
/// numbering.
struct Reader<'a> {
    styles: &'a docx_rs::Styles,
    numberings: &'a docx_rs::Numberings,
    /// Counter per level of each numbering instance; `None` until used or
    /// after a shallower level reset it.
    counters: HashMap<usize, [Option<u32>; LIST_LEVELS]>,
    bold: docx_rs::Bold,
    italic: docx_rs::Italic,
    no_underline: docx_rs::Underline,
    page_break: docx_rs::Break,
}

impl<'a> Reader<'a> {
    fn new(styles: &'a docx_rs::Styles, numberings: &'a docx_rs::Numberings) -> Self {
        Self {
            styles,
            numberings,
            counters: HashMap::new(),
            bold: docx_rs::Bold::new(),
            italic: docx_rs::Italic::new(),
            no_underline: docx_rs::Underline::new("none"),
            page_break: docx_rs::Break::new(docx_rs::BreakType::Page),
        }
    }

    fn blocks(&mut self, children: &[docx_rs::DocumentChild], out: &mut Vec<Block>) {
        use docx_rs::DocumentChild;
        for child in children {
            match child {
                DocumentChild::Paragraph(p) => out.push(Block::Paragraph(self.paragraph(p))),
                DocumentChild::Table(t) => out.push(Block::Table(self.table(t))),
                DocumentChild::StructuredDataTag(sdt) => self.sdt_blocks(sdt, out),
                DocumentChild::TableOfContents(toc) => {
                    for content in toc.before_contents.iter().chain(&toc.after_contents) {
                        match content {
                            docx_rs::TocContent::Paragraph(p) => {
                                out.push(Block::Paragraph(self.paragraph(p)));
                            }
                            docx_rs::TocContent::Table(t) => out.push(Block::Table(self.table(t))),
                        }
                    }
                }
                // Sections are writer only; bookmarks and comment ranges hold no content.
                DocumentChild::Section(_)
                | DocumentChild::BookmarkStart(_)
                | DocumentChild::BookmarkEnd(_)
                | DocumentChild::CommentStart(_)
                | DocumentChild::CommentEnd(_) => {}
            }
        }
    }

    fn sdt_blocks(&mut self, sdt: &docx_rs::StructuredDataTag, out: &mut Vec<Block>) {
        use docx_rs::StructuredDataTagChild as Child;
        for child in &sdt.children {
            match child {
                Child::Paragraph(p) => out.push(Block::Paragraph(self.paragraph(p))),
                Child::Table(t) => out.push(Block::Table(self.table(t))),
                Child::StructuredDataTag(inner) => self.sdt_blocks(inner, out),
                _ => {}
            }
        }
    }

    fn paragraph(&mut self, p: &docx_rs::Paragraph) -> Paragraph {
        let style = p.property.style.as_ref().map(|s| s.val.as_str());
        let mut runs = Vec::new();
        let mut page_break_after = false;
        self.inline(&p.children, style, &mut runs, &mut page_break_after);
        let text = runs.iter().map(|r| r.text.as_str()).collect();
        let heading_level = style.and_then(|id| self.heading_level(id));
        let numbering = p.property.numbering_property.as_ref().or_else(|| {
            style.and_then(|id| {
                self.style_chain(id).find_map(|s| s.paragraph_property.numbering_property.as_ref())
            })
        });
        let list = numbering.and_then(|np| self.list_item(np));
        Paragraph {
            style: style.map(str::to_owned),
            heading_level,
            list,
            runs,
            text,
            page_break_before: p.property.page_break_before.unwrap_or(false),
            page_break_after,
        }
    }

    /// Text of a paragraph inside a table cell.
    fn push_paragraph_text(&self, p: &docx_rs::Paragraph, out: &mut String) {
        let mut runs = Vec::new();
        let mut page_break = false;
        let style = p.property.style.as_ref().map(|s| s.val.as_str());
        self.inline(&p.children, style, &mut runs, &mut page_break);
        for run in &runs {
            out.push_str(&run.text);
        }
    }

    fn inline(
        &self,
        children: &[docx_rs::ParagraphChild],
        style: Option<&str>,
        runs: &mut Vec<Run>,
        page_break: &mut bool,
    ) {
        use docx_rs::ParagraphChild;
        for child in children {
            match child {
                ParagraphChild::Run(r) => runs.extend(self.run(r, style, page_break)),
                ParagraphChild::Insert(ins) => {
                    for c in &ins.children {
                        if let docx_rs::InsertChild::Run(r) = c {
                            runs.extend(self.run(r, style, page_break));
                        }
                    }
                }
                ParagraphChild::MoveTo(mv) => {
                    for c in &mv.children {
                        if let docx_rs::MoveToChild::Run(r) = c {
                            runs.extend(self.run(r, style, page_break));
                        }
                    }
                }
                ParagraphChild::Hyperlink(link) => self.inline(&link.children, style, runs, page_break),
                ParagraphChild::StructuredDataTag(sdt) => self.sdt_runs(sdt, style, runs, page_break),
                _ => {}
            }
        }
    }

    fn sdt_runs(
        &self,
        sdt: &docx_rs::StructuredDataTag,
        style: Option<&str>,
        runs: &mut Vec<Run>,
        page_break: &mut bool,
    ) {
        use docx_rs::StructuredDataTagChild as Child;
        for child in &sdt.children {
            match child {
                Child::Run(r) => runs.extend(self.run(r, style, page_break)),
                Child::StructuredDataTag(inner) => self.sdt_runs(inner, style, runs, page_break),
                _ => {}
            }
        }
    }

    /// A run with its text and resolved formatting; `None` when it holds no
    /// text (field codes, drawings, bookmarks).
    fn run(&self, r: &docx_rs::Run, style: Option<&str>, page_break: &mut bool) -> Option<Run> {
        use docx_rs::RunChild;
        let mut text = String::new();
        for child in &r.children {
            match child {
                RunChild::Text(t) => text.push_str(&t.text),
                RunChild::Tab(_) | RunChild::PTab(_) => text.push('\t'),
                RunChild::Break(b) => {
                    *page_break |= *b == self.page_break;
                    text.push('\n');
                }
                RunChild::CarriageReturn(_) => text.push('\n'),
                _ => {}
            }
        }
        if text.is_empty() {
            return None;
        }
        let rp = &r.run_property;
        let run_style = rp.style.as_ref().map(|s| s.val.as_str());
        let bold = self.flag(rp, run_style, style, |p| p.bold.as_ref().map(|b| *b == self.bold));
        let italic = self.flag(rp, run_style, style, |p| p.italic.as_ref().map(|i| *i == self.italic));
        let underline =
            self.flag(rp, run_style, style, |p| p.underline.as_ref().map(|u| *u != self.no_underline));
        Some(Run { text, bold, italic, underline })
    }

    /// A run property resolved from the run, then its character style, then
    /// the paragraph style, each through its `basedOn` chain.
    fn flag(
        &self,
        run: &docx_rs::RunProperty,
        run_style: Option<&str>,
        paragraph_style: Option<&str>,
        get: impl Fn(&docx_rs::RunProperty) -> Option<bool>,
    ) -> bool {
        get(run)
            .or_else(|| run_style.and_then(|id| self.style_chain(id).find_map(|s| get(&s.run_property))))
            .or_else(|| {
                paragraph_style.and_then(|id| self.style_chain(id).find_map(|s| get(&s.run_property)))
            })
            .unwrap_or(false)
    }

    /// The style `id` followed by the styles it is based on.
    fn style_chain(&self, id: &str) -> impl Iterator<Item = &'a docx_rs::Style> + use<'a> {
        let styles = self.styles;
        let mut next = styles.find_style_by_id(id);
        std::iter::from_fn(move || {
            let style = next?;
            next = style
                .based_on
                .as_ref()
                .and_then(serialized_str)
                .and_then(|base| styles.find_style_by_id(&base));
            Some(style)
        })
        .take(MAX_STYLE_DEPTH)
    }

    /// Heading level from the style's name, falling back to its id.
    fn heading_level(&self, id: &str) -> Option<u8> {
        self.styles
            .find_style_by_id(id)
            .and_then(|style| serialized_str(&style.name))
            .and_then(|name| heading_level_of(&name))
            .or_else(|| heading_level_of(id))
    }

    /// Counts the item in its numbering instance and renders its marker.
    fn list_item(&mut self, np: &docx_rs::NumberingProperty) -> Option<ListItem> {
        let num_id = np.id.as_ref()?.id;
        // numId 0 removes numbering inherited from the style.
        if num_id == 0 {
            return None;
        }
        let level = np.level.as_ref().map_or(0, |l| l.val).min(LIST_LEVELS - 1);

        let numberings = self.numberings;
        let numbering = numberings.numberings.iter().find(|n| n.id == num_id);
        let abstract_num = numbering
            .and_then(|n| numberings.abstract_nums.iter().find(|a| a.id == n.abstract_num_id));
        let override_of = |l: usize| numbering.and_then(|n| n.level_overrides.iter().find(|o| o.level == l));
        let definition = |l: usize| {
            override_of(l)
                .and_then(|o| o.override_level.as_ref())
                .or_else(|| abstract_num.and_then(|a| a.levels.iter().find(|d| d.level == l)))
        };
        let start = |l: usize| {
            override_of(l)
                .and_then(|o| o.override_start)
                .or_else(|| {
                    // `Start` keeps its value private; it serializes as a number.
                    definition(l)
                        .and_then(|d| serde_json::to_value(&d.start).ok())
                        .and_then(|v| v.as_u64())
                        .map(|v| v as usize)
                })
                .map_or(1, |v| u32::try_from(v).unwrap_or(u32::MAX))
        };

        let counters = self.counters.entry(num_id).or_insert([None; LIST_LEVELS]);
        counters[level] = Some(counters[level].map_or_else(|| start(level), |c| c.saturating_add(1)));
        for deeper in &mut counters[level + 1..] {
            *deeper = None;
        }
        let counters = *counters;

        let marker = match definition(level) {
            None => format!("{}.", counters[level].unwrap_or(1)),
            Some(def) => match def.format.val.as_str() {
                "bullet" => "•".to_owned(),
                "none" => String::new(),
                _ => {
                    let template = serialized_str(&def.text).unwrap_or_default();
                    let mut marker = String::with_capacity(template.len());
                    let mut chars = template.chars().peekable();
                    while let Some(c) = chars.next() {
                        let placeholder = chars
                            .peek()
                            .and_then(|d| d.to_digit(10))
                            .filter(|d| (1..=LIST_LEVELS as u32).contains(d));
                        match (c, placeholder) {
                            ('%', Some(d)) => {
                                chars.next();
                                let l = d as usize - 1;
                                let n = counters[l].unwrap_or_else(|| start(l));
                                let format = definition(l).map_or("decimal", |lvl| lvl.format.val.as_str());
                                marker.push_str(&format_number(n, format));
                            }
                            _ => marker.push(c),
                        }
                    }
                    marker
                }
            },
        };
        Some(ListItem { num_id, level: level as u8, marker })
    }

    fn table(&self, t: &docx_rs::Table) -> Table {
        let rows = t
            .rows
            .iter()
            .map(|docx_rs::TableChild::TableRow(row)| {
                row.cells
                    .iter()
                    .map(|docx_rs::TableRowChild::TableCell(cell)| self.cell_text(cell))
                    .collect()
            })
            .collect();
        Table { rows }
    }

    fn cell_text(&self, cell: &docx_rs::TableCell) -> String {
        use docx_rs::TableCellContent;
        let mut out = String::new();
        for (i, content) in cell.children.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            match content {
                TableCellContent::Paragraph(p) => self.push_paragraph_text(p, &mut out),
                TableCellContent::Table(t) => push_table_text(&self.table(t), &mut out),
                TableCellContent::StructuredDataTag(sdt) => self.push_sdt_text(sdt, &mut out),
                TableCellContent::TableOfContents(_) => {}
            }
        }
        out
    }

    fn push_sdt_text(&self, sdt: &docx_rs::StructuredDataTag, out: &mut String) {
        use docx_rs::StructuredDataTagChild as Child;
        for child in &sdt.children {
            match child {
                Child::Paragraph(p) => self.push_paragraph_text(p, out),
                Child::Table(t) => push_table_text(&self.table(t), out),
                Child::StructuredDataTag(inner) => self.push_sdt_text(inner, out),
                _ => {}
            }
        }
    }
}

/// Flows the outline onto pages.
struct Layout<'f> {
    fonts: &'f Fonts,
    pages: Vec<Vec<Op>>,
    page: PageSize,
    margins: Margins,
    y: f32,
}

impl Layout<'_> {
    fn content_width(&self) -> f32 {
        (self.page.width - self.margins.left - self.margins.right).max(1.0)
    }

    fn bottom(&self) -> f32 {
        self.page.height - self.margins.bottom
    }

    fn new_page(&mut self) {
        self.pages.push(Vec::new());
        self.y = self.margins.top;
    }

    /// Starts a new page when `height` does not fit below the cursor, unless
    /// the cursor is already at the top (then the content overflows).
    fn reserve(&mut self, height: f32) {
        if self.y + height > self.bottom() && self.y > self.margins.top {
            self.new_page();
        }
    }

    fn push(&mut self, op: Op) {
        if let Some(page) = self.pages.last_mut() {
            page.push(op);
        }
    }

    fn page_is_empty(&self) -> bool {
        self.pages.last().is_none_or(Vec::is_empty)
    }

    fn paragraph(&mut self, block: usize, p: &Paragraph) {
        if p.page_break_before && !self.page_is_empty() {
            self.new_page();
        }
        let (size, heading) = match p.heading_level {
            Some(0) => (26.0, true),
            Some(1) => (20.0, true),
            Some(2) => (16.0, true),
            Some(3) => (14.0, true),
            Some(_) => (12.0, true),
            None => (BODY_SIZE, false),
        };
        if heading && self.y > self.margins.top {
            self.y += size * 0.5;
        }
        let indent = p.list.as_ref().map_or(0.0, |l| LIST_INDENT * (f32::from(l.level) + 1.0));
        let width = (self.content_width() - indent).max(size);
        let line_height = self.fonts.line_height(size);
        let x = self.margins.left + indent;
        for (i, line) in self.fonts.wrap(&p.text, size, heading, width).into_iter().enumerate() {
            self.reserve(line_height);
            if i == 0 && p.list.is_some() {
                self.push(Op::Marker { x: x - LIST_INDENT, y: self.y, size, block });
            }
            if !line.is_empty() {
                // `wrap` returns subslices of `p.text`.
                let start = (line.as_ptr() as usize).saturating_sub(p.text.as_ptr() as usize);
                let source = Source::Paragraph { block, heading };
                self.push(Op::Text { x, y: self.y, size, source, start, end: start + line.len() });
            }
            self.y += line_height;
        }
        self.y += size * 0.5;
        if p.page_break_after {
            self.new_page();
        }
    }

    fn table(&mut self, block: usize, t: &Table) {
        let cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let col_width = self.content_width() / cols as f32;
        let text_width = (col_width - 2.0 * CELL_PADDING).max(1.0);
        let size = BODY_SIZE;
        let line_height = self.fonts.line_height(size);
        for (r, row) in t.rows.iter().enumerate() {
            let lines: Vec<Vec<&str>> =
                row.iter().map(|cell| self.fonts.wrap(cell, size, false, text_width)).collect();
            let row_lines = lines.iter().map(Vec::len).max().unwrap_or(1).max(1);
            let row_height = row_lines as f32 * line_height + 2.0 * CELL_PADDING;
            self.reserve(row_height);
            for c in 0..cols {
                let x = self.margins.left + c as f32 * col_width;
                self.push(Op::Border { x, y: self.y, w: col_width, h: row_height });
                let (Some(cell), Some(cell_lines)) = (row.get(c), lines.get(c)) else { continue };
                for (i, line) in cell_lines.iter().enumerate() {
                    if line.is_empty() {
                        continue;
                    }
                    // `wrap` returns subslices of `cell`.
                    let start = (line.as_ptr() as usize).saturating_sub(cell.as_ptr() as usize);
                    self.push(Op::Text {
                        x: x + CELL_PADDING,
                        y: self.y + CELL_PADDING + i as f32 * line_height,
                        size,
                        source: Source::Cell { block, row: r, col: c },
                        start,
                        end: start + line.len(),
                    });
                }
            }
            self.y += row_height;
        }
        self.y += size * 0.5;
    }
}

/// Lays the outline out on pages; there is always at least one page.
fn layout(outline: &Outline, page: PageSize, margins: Margins, fonts: &Fonts) -> Vec<Vec<Op>> {
    let mut layout = Layout { fonts, pages: vec![Vec::new()], page, margins, y: margins.top };
    for (block, content) in outline.blocks.iter().enumerate() {
        match content {
            Block::Paragraph(p) => layout.paragraph(block, p),
            Block::Table(t) => layout.table(block, t),
        }
    }
    // A trailing page break leaves an empty last page Word would not show.
    if layout.pages.len() > 1 && layout.page_is_empty() {
        layout.pages.pop();
    }
    layout.pages
}

#[cfg(test)]
mod tests {
    use super::*;
    use docx_rs::{
        AbstractNumbering, BreakType, Docx, IndentLevel, Level, LevelJc, LevelText, NumberFormat,
        Numbering, NumberingId, PageMargin, Start, Style, StyleType, TableCell, TableRow,
    };
    use std::io::Cursor;

    fn pack(docx: Docx) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        docx.build().pack(&mut out).unwrap();
        out.into_inner()
    }

    fn para(text: &str) -> docx_rs::Paragraph {
        docx_rs::Paragraph::new().add_run(docx_rs::Run::new().add_text(text))
    }

    fn paragraphs(doc: &DocxDoc) -> Vec<&Paragraph> {
        doc.outline()
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => Some(p),
                Block::Table(_) => None,
            })
            .collect()
    }

    fn cell(text: &str) -> TableCell {
        TableCell::new().add_paragraph(para(text))
    }

    fn table_2x2() -> docx_rs::Table {
        docx_rs::Table::new(vec![
            TableRow::new(vec![cell("a1"), cell("b1")]),
            TableRow::new(vec![cell("a2"), cell("b2")]),
        ])
    }

    fn is_dark(p: [u8; 4]) -> bool {
        p[0] < 128 && p[3] > 0
    }

    #[test]
    fn headings_come_from_style_names_and_ids() {
        let docx = Docx::new()
            .add_style(Style::new("Heading1", StyleType::Paragraph).name("heading 1"))
            .add_style(Style::new("Custom", StyleType::Paragraph).name("heading 2"))
            .add_paragraph(para("Report").style("Title"))
            .add_paragraph(para("Intro").style("Heading1"))
            .add_paragraph(para("Body text"))
            .add_paragraph(para("Details").style("Custom"))
            .add_paragraph(para("Deep").style("Heading3"));
        let doc = DocxDoc::parse(&pack(docx)).unwrap();
        assert_eq!(
            doc.headings(),
            [
                (0, "Report".to_owned()),
                (1, "Intro".to_owned()),
                (2, "Details".to_owned()),
                (3, "Deep".to_owned())
            ]
        );
        let body = paragraphs(&doc)[2];
        assert_eq!(body.heading_level, None);
        assert_eq!(paragraphs(&doc)[3].style.as_deref(), Some("Custom"));
    }

    #[test]
    fn runs_keep_their_formatting() {
        let docx = Docx::new().add_paragraph(
            docx_rs::Paragraph::new()
                .add_run(docx_rs::Run::new().add_text("plain "))
                .add_run(docx_rs::Run::new().add_text("strong").bold())
                .add_run(docx_rs::Run::new().add_text(" em").italic().underline("single")),
        );
        let doc = DocxDoc::parse(&pack(docx)).unwrap();
        let p = paragraphs(&doc)[0];
        let formats: Vec<_> = p.runs.iter().map(|r| (r.text.as_str(), r.bold, r.italic, r.underline)).collect();
        assert_eq!(
            formats,
            [("plain ", false, false, false), ("strong", true, false, false), (" em", false, true, true)]
        );
        assert_eq!(p.text, "plain strong em");
    }

    #[test]
    fn numbered_list_items_carry_level_and_marker() {
        let level = |l: usize, format: &str, text: &str| {
            Level::new(l, Start::new(1), NumberFormat::new(format), LevelText::new(text), LevelJc::new("left"))
        };
        let item = |text: &str, l: usize| para(text).numbering(NumberingId::new(2), IndentLevel::new(l));
        let docx = Docx::new()
            .add_abstract_numbering(
                // docx-rs always writes its own abstract numbering 1.
                AbstractNumbering::new(5)
                    .add_level(level(0, "decimal", "%1."))
                    .add_level(level(1, "lowerLetter", "%1.%2)")),
            )
            .add_numbering(Numbering::new(2, 5))
            .add_paragraph(item("first", 0))
            .add_paragraph(item("second", 0))
            .add_paragraph(item("sub", 1))
            .add_paragraph(item("third", 0))
            .add_paragraph(item("sub again", 1))
            .add_paragraph(para("after"));
        let doc = DocxDoc::parse(&pack(docx)).unwrap();
        let items: Vec<_> = paragraphs(&doc)
            .iter()
            .map(|p| p.list.as_ref().map(|l| (l.num_id, l.level, l.marker.clone())))
            .collect();
        assert_eq!(
            items,
            [
                Some((2, 0, "1.".to_owned())),
                Some((2, 0, "2.".to_owned())),
                Some((2, 1, "2.a)".to_owned())),
                Some((2, 0, "3.".to_owned())),
                Some((2, 1, "3.a)".to_owned())),
                None
            ]
        );
    }

    #[test]
    fn table_cells_and_text_keep_document_order() {
        let docx = Docx::new()
            .add_paragraph(para("before"))
            .add_table(table_2x2())
            .add_paragraph(
                docx_rs::Paragraph::new().add_run(
                    docx_rs::Run::new()
                        .add_text("x")
                        .add_tab()
                        .add_text("y")
                        .add_break(BreakType::TextWrapping)
                        .add_text("z"),
                ),
            );
        let doc = DocxDoc::parse(&pack(docx)).unwrap();
        let Block::Table(table) = &doc.outline().blocks[1] else { panic!("expected a table") };
        assert_eq!(table.rows, [["a1", "b1"], ["a2", "b2"]]);
        assert_eq!(doc.text(), "before\na1\tb1\na2\tb2\nx\ty\nz");
    }

    #[test]
    fn page_size_and_margins_come_from_the_section() {
        let docx = Docx::new()
            .page_size(8000, 10000)
            .page_margin(PageMargin::new().top(1440).left(1440).right(1440).bottom(1440))
            .add_paragraph(para("hello"));
        let doc = DocxDoc::parse(&pack(docx)).unwrap();
        assert_eq!(doc.page_count(), 1);
        assert_eq!(doc.page_size(0).unwrap(), PageSize { width: 400.0, height: 500.0 });
        assert!(matches!(doc.page_size(1), Err(DocsError::PageRange { index: 1, count: 1 })));
    }

    #[test]
    fn long_documents_paginate_and_page_breaks_start_pages() {
        let mut docx = Docx::new().page_size(12240, 15840);
        for i in 0..200 {
            docx = docx.add_paragraph(para(&format!("Paragraph number {i}")));
        }
        let doc = DocxDoc::parse(&pack(docx)).unwrap();
        assert!(doc.page_count() > 1, "{} pages", doc.page_count());
        let last = doc.page_count() - 1;
        assert!(doc.render_page(last, 1.0, &Fonts::none()).is_ok());
        assert!(matches!(
            doc.render_page(last + 1, 1.0, &Fonts::none()),
            Err(DocsError::PageRange { .. })
        ));

        let docx = Docx::new()
            .add_paragraph(para("one"))
            .add_paragraph(para("two").page_break_before(true))
            .add_paragraph(docx_rs::Paragraph::new().add_run(docx_rs::Run::new().add_break(BreakType::Page)))
            .add_paragraph(para("three"));
        let doc = DocxDoc::parse(&pack(docx)).unwrap();
        assert_eq!(doc.page_count(), 3);
    }

    #[test]
    fn render_draws_text_inside_the_margins() {
        let docx = Docx::new()
            .page_size(8000, 10000)
            .page_margin(PageMargin::new().top(1440).left(1440).right(1440).bottom(1440))
            .add_style(Style::new("Heading1", StyleType::Paragraph).name("heading 1"))
            .add_paragraph(para("Title text").style("Heading1"))
            .add_paragraph(para("Body"))
            .add_table(table_2x2());
        let doc = DocxDoc::parse(&pack(docx)).unwrap();
        let image = doc.render_page(0, 2.0, &Fonts::none()).unwrap();
        assert_eq!((image.width, image.height), (800, 1000));
        let dark_in = |x0: u32, y0: u32, x1: u32, y1: u32| {
            (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).any(|(x, y)| {
                image.pixel(x, y).is_some_and(is_dark)
            })
        };
        // Heading starts at the 72pt margin, scaled by 2.
        assert!(dark_in(144, 144, 344, 210));
        assert!(!dark_in(0, 0, 800, 140), "nothing above the top margin");
        assert!(!dark_in(0, 0, 140, 1000), "nothing left of the left margin");
    }

    #[test]
    fn garbage_is_an_invalid_docx() {
        assert!(matches!(
            DocxDoc::parse(b"not a zip archive"),
            Err(DocsError::Invalid { format: Format::Docx, .. })
        ));
    }
}
