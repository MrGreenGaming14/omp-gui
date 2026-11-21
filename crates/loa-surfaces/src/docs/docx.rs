//! DOCX documents read with `docx-rs`: an owned outline for the agent (paragraphs with heading level, list
//! numbering and run formatting, and tables) and a flow layout renderer, not Word's: blocks run top to bottom
//! inside the margins, headings bigger and bold, list items indented under a marker, tables as equal width cells.

use crate::docs::raster::{Canvas, Fonts, TextStyle};
use crate::docs::{DataQuery, DocTypography, DocsError, Format, PageSize, Paged, RenderOpts, Result, Rgba, json, page_at};
use docx_rs::StructuredDataTagChild as SdtChild;
use std::borrow::Cow;
use std::collections::HashMap;

/// Page used when the document has no section properties: US Letter.
const LETTER: PageSize = PageSize { width: 612.0, height: 792.0 };
/// Margin on every side when the document has no section properties.
const DEFAULT_MARGIN: f32 = 72.0;
const TWIPS_PER_POINT: f32 = 20.0;
/// Indent per list level in points.
const LIST_INDENT: f32 = 18.0;
/// Padding between a table cell's border and its text, in points.
const CELL_PADDING: f32 = 4.0;
/// Advance of a tab, in ems. Matches the box metric the layout is made with.
const TAB_EM: f32 = 0.6;
/// Font sizes of the title and headings 1 to 3; deeper headings use 12 pt.
const HEADING_SIZES: [f32; 4] = [26.0, 20.0, 16.0, 14.0];
/// List levels OOXML defines (`ilvl` 0 to 8).
const LIST_LEVELS: usize = 9;
/// Level placeholders of a list marker template (`w:lvlText`).
const LEVEL_PLACEHOLDERS: [&str; LIST_LEVELS] = ["%1", "%2", "%3", "%4", "%5", "%6", "%7", "%8", "%9"];
/// Bound on `basedOn` chains, which a malformed file can make cyclic.
const MAX_STYLE_DEPTH: usize = 16;

/// The document's content in reading order.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Outline {
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
}

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

#[derive(Copy, Clone, Debug, PartialEq)]
struct Margins {
    top: f32,
    right: f32,
    bottom: f32,
    left: f32,
}

/// A parsed DOCX file. Pagination is computed once at parse time with the box metrics of [`Fonts::none`], so
/// page count and renders agree whatever fonts the viewer passes; real glyphs are narrower than the boxes. A
/// table row taller than a page is clipped. All pages take the size of the document's final section.
#[derive(Clone, Debug)]
pub struct DocxDoc {
    outline: Outline,
    page: PageSize,
    pages: Vec<Vec<Op>>,
}

/// A drawing operation at scale 1, in points.
#[derive(Copy, Clone, Debug)]
enum Op {
    /// Bytes `start..end` of the paragraph at `block`, or of its `(row, col)` cell, top edge at `y`.
    Text { x: f32, y: f32, size: f32, block: usize, cell: Option<(usize, usize)>, start: usize, end: usize },
    /// The list marker of the paragraph at `block`.
    Marker { x: f32, y: f32, size: f32, block: usize },
    Border { x: f32, y: f32, w: f32, h: f32 },
}

impl DocxDoc {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        // docx-rs indexes attribute lists without checking them (`<w:u/>`, `<w:start/>`), so a malformed part
        // panics inside the reader.
        let docx = std::panic::catch_unwind(|| docx_rs::read_docx(bytes))
            .map_err(|_| DocsError::invalid(Format::Docx, "malformed document part"))?
            .map_err(|e| DocsError::invalid(Format::Docx, e))?;
        let (styles, numberings, no_underline) = (&docx.styles, &docx.numberings, docx_rs::Underline::new("none"));
        let mut reader = Reader { styles, numberings, counters: HashMap::new(), no_underline };
        let outline = Outline { blocks: reader.blocks(&docx.document.children) };
        let (page, margins) = section_geometry(&docx.document.section_property);
        let pages = layout(&outline, page, margins, &Fonts::none());
        Ok(Self { outline, page, pages })
    }

    /// Plain text: paragraphs joined by `\n`, table cells by `\t`, rows by `\n`.
    pub fn text(&self) -> String {
        (self.outline.blocks.iter())
            .map(|block| match block {
                Block::Paragraph(p) => Cow::Borrowed(p.text.as_str()),
                Block::Table(t) => Cow::Owned(table_text(t)),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn draw_op(&self, canvas: &mut Canvas, fonts: &Fonts, scale: f32, opts: &RenderOpts, op: &Op) {
        let s = |v: f32| v * scale;
        match *op {
            Op::Border { x, y, w, h } => canvas.stroke_rect(s(x), s(y), s(w), s(h), scale, opts.palette.rule),
            Op::Marker { x, y, size, block } => {
                if let Some(Block::Paragraph(Paragraph { list: Some(item), .. })) = self.outline.blocks.get(block) {
                    draw_segment(canvas, fonts, opts.text(s(size)), s(x), s(y), &item.marker, false);
                }
            }
            Op::Text { x, y, size, block, cell, start, end } => {
                let (style, y) = (opts.text(s(size)), s(y));
                match (self.outline.blocks.get(block), cell) {
                    // Run by run, so bold and underline follow the runs.
                    (Some(Block::Paragraph(p)), None) => {
                        let (mut caret, mut run_start) = (s(x), 0);
                        for run in &p.runs {
                            let (a, b) = (start.max(run_start), end.min(run_start + run.text.len()));
                            run_start += run.text.len();
                            if let Some(segment) = p.text.get(a..b) {
                                let style = style.bold(p.heading_level.is_some() || run.bold);
                                caret += draw_segment(canvas, fonts, style, caret, y, segment, run.underline);
                            }
                        }
                    }
                    (Some(Block::Table(t)), Some((row, col))) => {
                        if let Some(line) = t.rows.get(row).and_then(|r| r.get(col)).and_then(|c| c.get(start..end)) {
                            draw_segment(canvas, fonts, style, s(x), y, line, false);
                        }
                    }
                    _ => {}
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
        page_at(&self.pages, index).map(|_| self.page)
    }

    fn query(&self, query: &DataQuery) -> Option<Result<serde_json::Value>> {
        match query {
            DataQuery::Text { page: None } => Some(json(&self.text())),
            DataQuery::Outline => Some(json(&self.outline)),
            _ => None,
        }
    }

    fn render(&self, index: usize, scale: f32, fonts: &Fonts, opts: &RenderOpts) -> Result<Rgba> {
        let ops = page_at(&self.pages, index)?;
        let (width, height) = self.page.pixels(scale);
        let mut canvas = Canvas::new(width, height, opts.palette.page);
        for op in ops {
            self.draw_op(&mut canvas, fonts, scale, opts, op);
        }
        Ok(canvas.into_rgba())
    }
}

/// Draws one line piece, advancing tabs by [`TAB_EM`]. Returns the advance.
fn draw_segment(canvas: &mut Canvas, fonts: &Fonts, style: TextStyle, x: f32, y: f32, text: &str, underline: bool) -> f32 {
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
        canvas.fill_rect(x, y + style.size * 1.05, advance, (style.size / 14.0).max(1.0), style.color);
    }
    advance
}

fn table_text(table: &Table) -> String {
    table.rows.iter().map(|row| row.join("\t")).collect::<Vec<_>>().join("\n")
}

/// Page size and margins of the body's section properties.
///
/// docx-rs fills a missing `w:sectPr` with its own defaults, so a section property identical to those defaults
/// is taken as absent and replaced by Letter with 1 inch margins.
fn section_geometry(section: &docx_rs::SectionProperty) -> (PageSize, Margins) {
    let m = DEFAULT_MARGIN;
    let default = (LETTER, Margins { top: m, right: m, bottom: m, left: m });
    if *section == docx_rs::SectionProperty::new() {
        return default;
    }
    // `PageSize` keeps its twips private; its serialization is `{w, h, orient}`.
    let Some((w, h)) = serde_json::to_value(&section.page_size)
        .ok()
        .and_then(|v| Some((v.get("w")?.as_u64()?, v.get("h")?.as_u64()?)))
        .filter(|&(w, h)| w > 0 && h > 0)
    else {
        return default;
    };
    let page = PageSize { width: w as f32 / TWIPS_PER_POINT, height: h as f32 / TWIPS_PER_POINT };
    let m = &section.page_margin;
    // Negative margins let text overlap the header; clamp so the text area keeps at least a third of the page.
    let margin = |twips: i32, extent: f32| (twips.max(0) as f32 / TWIPS_PER_POINT).min(extent / 3.0);
    let (top, bottom) = (margin(m.top, page.height), margin(m.bottom, page.height));
    (page, Margins { top, right: margin(m.right, page.width), bottom, left: margin(m.left, page.width) })
}

/// The string a docx-rs value serializes to, for types with private fields (`Name`, `BasedOn`, `LevelText`).
fn serialized_str<T: serde::Serialize>(value: &T) -> Option<String> {
    serde_json::to_value(value).ok()?.as_str().map(str::to_owned)
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
    const NUMERALS: [(u32, &str); 13] = [
        (1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"),
        (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I"),
    ];
    match format {
        "lowerLetter" | "upperLetter" if n > 0 => {
            let letter = char::from(b'a' + ((n - 1) % 26) as u8);
            let letters: String = std::iter::repeat_n(letter, ((n - 1) / 26 + 1) as usize).collect();
            if format == "upperLetter" { letters.to_ascii_uppercase() } else { letters }
        }
        "lowerRoman" | "upperRoman" if n > 0 => {
            let (mut rest, mut roman) = (n, String::new());
            for (value, numeral) in NUMERALS {
                while rest >= value {
                    roman.push_str(numeral);
                    rest -= value;
                }
            }
            if format == "upperRoman" { roman } else { roman.to_ascii_lowercase() }
        }
        "decimalZero" => format!("{n:02}"),
        _ => n.to_string(),
    }
}

/// Calls `f` on the non-tag children of `sdt`, flattening nested tags.
fn sdt_leaves<'s>(sdt: &'s docx_rs::StructuredDataTag, f: &mut dyn FnMut(&'s SdtChild)) {
    for child in &sdt.children {
        match child {
            SdtChild::StructuredDataTag(inner) => sdt_leaves(inner, f),
            leaf => f(leaf),
        }
    }
}

/// Calls `f` on the runs of `children`, through insertions, moves, hyperlinks and content controls.
fn each_run<'d>(children: &'d [docx_rs::ParagraphChild], f: &mut dyn FnMut(&'d docx_rs::Run)) {
    use docx_rs::{InsertChild, MoveToChild, ParagraphChild as P};
    for child in children {
        match child {
            P::Run(r) => f(r),
            P::Insert(ins) => ins.children.iter().for_each(|c| if let InsertChild::Run(r) = c { f(r) }),
            P::MoveTo(mv) => mv.children.iter().for_each(|c| if let MoveToChild::Run(r) = c { f(r) }),
            P::Hyperlink(link) => each_run(&link.children, f),
            P::StructuredDataTag(sdt) => sdt_leaves(sdt, &mut |leaf| if let SdtChild::Run(r) = leaf { f(r) }),
            _ => {}
        }
    }
}

/// Builds the outline from the docx-rs tree, resolving styles and list numbering.
struct Reader<'a> {
    styles: &'a docx_rs::Styles,
    numberings: &'a docx_rs::Numberings,
    /// Counter per level of each numbering instance; `None` until used or after a shallower level reset it.
    counters: HashMap<usize, [Option<u32>; LIST_LEVELS]>,
    no_underline: docx_rs::Underline,
}

impl<'a> Reader<'a> {
    fn blocks(&mut self, children: &[docx_rs::DocumentChild]) -> Vec<Block> {
        use docx_rs::{DocumentChild as D, TocContent};
        let mut out = Vec::new();
        for child in children {
            match child {
                D::Paragraph(p) => out.push(Block::Paragraph(self.paragraph(p))),
                D::Table(t) => out.push(Block::Table(self.table(t))),
                D::StructuredDataTag(sdt) => sdt_leaves(sdt, &mut |leaf| match leaf {
                    SdtChild::Paragraph(p) => out.push(Block::Paragraph(self.paragraph(p))),
                    SdtChild::Table(t) => out.push(Block::Table(self.table(t))),
                    _ => {}
                }),
                D::TableOfContents(toc) => {
                    out.extend(toc.before_contents.iter().chain(&toc.after_contents).map(|content| match content {
                        TocContent::Paragraph(p) => Block::Paragraph(self.paragraph(p)),
                        TocContent::Table(t) => Block::Table(self.table(t)),
                    }));
                }
                // Sections are writer only; bookmarks and comment ranges hold no content.
                _ => {}
            }
        }
        out
    }

    fn paragraph(&mut self, p: &docx_rs::Paragraph) -> Paragraph {
        let style = p.property.style.as_ref().map(|s| s.val.as_str());
        let (runs, page_break_after) = self.runs(p);
        let numbering = (p.property.numbering_property.as_ref())
            .or_else(|| self.style_chain(style).find_map(|s| s.paragraph_property.numbering_property.as_ref()));
        Paragraph {
            style: style.map(str::to_owned),
            // From the style's name, falling back to its id.
            heading_level: style.and_then(|id| {
                let name = self.styles.find_style_by_id(id).and_then(|style| serialized_str(&style.name));
                name.and_then(|name| heading_level_of(&name)).or_else(|| heading_level_of(id))
            }),
            list: numbering.and_then(|np| self.list_item(np)),
            text: runs.iter().map(|r| r.text.as_str()).collect(),
            runs,
            page_break_before: p.property.page_break_before.unwrap_or(false),
            page_break_after,
        }
    }

    /// The paragraph's runs with their text, and whether it holds a page break.
    fn runs(&self, p: &docx_rs::Paragraph) -> (Vec<Run>, bool) {
        let style = p.property.style.as_ref().map(|s| s.val.as_str());
        let (mut runs, mut page_break) = (Vec::new(), false);
        each_run(&p.children, &mut |r| runs.extend(self.run(r, style, &mut page_break)));
        (runs, page_break)
    }

    /// A run with its text and resolved formatting; `None` when it holds no text (field codes, drawings).
    fn run(&self, r: &docx_rs::Run, paragraph_style: Option<&str>, page_break: &mut bool) -> Option<Run> {
        use docx_rs::RunChild as R;
        let mut text = String::new();
        for child in &r.children {
            match child {
                R::Text(t) => text.push_str(&t.text),
                R::Tab(_) | R::PTab(_) => text.push('\t'),
                R::Break(b) => {
                    *page_break |= *b == docx_rs::Break::new(docx_rs::BreakType::Page);
                    text.push('\n');
                }
                R::CarriageReturn(_) => text.push('\n'),
                _ => {}
            }
        }
        if text.is_empty() {
            return None;
        }
        // Each property comes from the run, else its character style, else the paragraph style, each through
        // its `basedOn` chain.
        let rp = &r.run_property;
        let inherited =
            self.style_chain(rp.style.as_ref().map(|s| s.val.as_str())).chain(self.style_chain(paragraph_style));
        let flag = |get: &dyn Fn(&docx_rs::RunProperty) -> Option<bool>| {
            get(rp).or_else(|| inherited.clone().find_map(|s| get(&s.run_property))).unwrap_or(false)
        };
        Some(Run {
            text,
            bold: flag(&|p| p.bold.as_ref().map(|b| *b == docx_rs::Bold::new())),
            italic: flag(&|p| p.italic.as_ref().map(|i| *i == docx_rs::Italic::new())),
            underline: flag(&|p| p.underline.as_ref().map(|u| *u != self.no_underline)),
        })
    }

    /// The style `id` followed by the styles it is based on.
    fn style_chain(&self, id: Option<&str>) -> impl Iterator<Item = &'a docx_rs::Style> + Clone + use<'a> {
        let styles = self.styles;
        let mut next = id.and_then(|id| styles.find_style_by_id(id));
        std::iter::from_fn(move || {
            let style = next?;
            next = style.based_on.as_ref().and_then(serialized_str).and_then(|base| styles.find_style_by_id(&base));
            Some(style)
        })
        .take(MAX_STYLE_DEPTH)
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
        let abstract_num = numbering.and_then(|n| numberings.abstract_nums.iter().find(|a| a.id == n.abstract_num_id));
        let override_of = |l: usize| numbering.and_then(|n| n.level_overrides.iter().find(|o| o.level == l));
        let definition = |l: usize| {
            (override_of(l).and_then(|o| o.override_level.as_ref()))
                .or_else(|| abstract_num.and_then(|a| a.levels.iter().find(|d| d.level == l)))
        };
        let start = |l: usize| {
            (override_of(l).and_then(|o| o.override_start))
                // `Start` keeps its value private; it serializes as a number.
                .or_else(|| definition(l).and_then(|d| serde_json::to_value(&d.start).ok()?.as_u64()).map(|v| v as usize))
                .map_or(1, |v| u32::try_from(v).unwrap_or(u32::MAX))
        };

        let counters = self.counters.entry(num_id).or_insert([None; LIST_LEVELS]);
        counters[level] = Some(counters[level].map_or_else(|| start(level), |c| c.saturating_add(1)));
        counters[level + 1..].fill(None);
        let counters = *counters;

        let marker = match definition(level) {
            None => format!("{}.", counters[level].unwrap_or(1)),
            Some(def) if def.format.val == "bullet" => "•".to_owned(),
            Some(def) if def.format.val == "none" => String::new(),
            Some(def) => {
                let mut marker = serialized_str(&def.text).unwrap_or_default();
                for (l, placeholder) in LEVEL_PLACEHOLDERS.iter().enumerate() {
                    if marker.contains(placeholder) {
                        let format = definition(l).map_or("decimal", |lvl| lvl.format.val.as_str());
                        let number = format_number(counters[l].unwrap_or_else(|| start(l)), format);
                        marker = marker.replace(placeholder, &number);
                    }
                }
                marker
            }
        };
        Some(ListItem { num_id, level: level as u8, marker })
    }

    fn table(&self, t: &docx_rs::Table) -> Table {
        let rows = (t.rows.iter())
            .map(|docx_rs::TableChild::TableRow(row)| {
                row.cells.iter().map(|docx_rs::TableRowChild::TableCell(cell)| self.cell_text(cell)).collect()
            })
            .collect();
        Table { rows }
    }

    fn cell_text(&self, cell: &docx_rs::TableCell) -> String {
        use docx_rs::TableCellContent as C;
        let plain = |p: &docx_rs::Paragraph| self.runs(p).0.into_iter().map(|r| r.text).collect::<String>();
        (cell.children.iter())
            .map(|content| match content {
                C::Paragraph(p) => plain(p),
                C::Table(t) => table_text(&self.table(t)),
                C::StructuredDataTag(sdt) => {
                    let mut out = String::new();
                    sdt_leaves(sdt, &mut |leaf| match leaf {
                        SdtChild::Paragraph(p) => out.push_str(&plain(p)),
                        SdtChild::Table(t) => out.push_str(&table_text(&self.table(t))),
                        _ => {}
                    });
                    out
                }
                C::TableOfContents(_) => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Byte range of `line` in `source`; `wrap` returns subslices of its text.
fn span(source: &str, line: &str) -> (usize, usize) {
    let start = (line.as_ptr() as usize).saturating_sub(source.as_ptr() as usize);
    (start, start + line.len())
}

struct Layout<'f> {
    fonts: &'f Fonts,
    pages: Vec<Vec<Op>>,
    margins: Margins,
    /// Width and bottom edge of the text area.
    width: f32,
    bottom: f32,
    y: f32,
}

impl Layout<'_> {
    fn new_page(&mut self) {
        self.pages.push(Vec::new());
        self.y = self.margins.top;
    }

    /// Takes `height` below the cursor, on a new page when it doesn't fit unless the cursor is already at the top
    /// (then it overflows). Returns the top of the taken space.
    fn advance(&mut self, height: f32) -> f32 {
        if self.y + height > self.bottom && self.y > self.margins.top {
            self.new_page();
        }
        self.y += height;
        self.y - height
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
        let heading = p.heading_level.is_some();
        let size = (p.heading_level)
            .map_or(DocTypography::default().body, |l| HEADING_SIZES.get(usize::from(l)).copied().unwrap_or(12.0));
        if heading && self.y > self.margins.top {
            self.y += size * 0.5;
        }
        let indent = p.list.as_ref().map_or(0.0, |l| LIST_INDENT * (f32::from(l.level) + 1.0));
        let width = (self.width - indent).max(size);
        let line_height = self.fonts.line_height(size);
        let x = self.margins.left + indent;
        for (i, line) in self.fonts.wrap(&p.text, size, heading, width).into_iter().enumerate() {
            let y = self.advance(line_height);
            if i == 0 && p.list.is_some() {
                self.push(Op::Marker { x: x - LIST_INDENT, y, size, block });
            }
            if !line.is_empty() {
                let (start, end) = span(&p.text, line);
                self.push(Op::Text { x, y, size, block, cell: None, start, end });
            }
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
        let col_width = self.width / cols as f32;
        let text_width = (col_width - 2.0 * CELL_PADDING).max(1.0);
        let size = DocTypography::default().body;
        let line_height = self.fonts.line_height(size);
        for (r, row) in t.rows.iter().enumerate() {
            let lines: Vec<Vec<&str>> = row.iter().map(|cell| self.fonts.wrap(cell, size, false, text_width)).collect();
            let row_height = lines.iter().map(Vec::len).max().unwrap_or(1).max(1) as f32 * line_height + 2.0 * CELL_PADDING;
            let y = self.advance(row_height);
            for c in 0..cols {
                let x = self.margins.left + c as f32 * col_width;
                self.push(Op::Border { x, y, w: col_width, h: row_height });
                let (Some(cell), Some(cell_lines)) = (row.get(c), lines.get(c)) else { continue };
                for (i, line) in cell_lines.iter().enumerate().filter(|(_, line)| !line.is_empty()) {
                    let (start, end) = span(cell, line);
                    let (x, y) = (x + CELL_PADDING, y + CELL_PADDING + i as f32 * line_height);
                    self.push(Op::Text { x, y, size, block, cell: Some((r, c)), start, end });
                }
            }
        }
        self.y += size * 0.5;
    }
}

/// Lays the outline out on pages; there is always at least one page.
fn layout(outline: &Outline, page: PageSize, margins: Margins, fonts: &Fonts) -> Vec<Vec<Op>> {
    let width = (page.width - margins.left - margins.right).max(1.0);
    let bottom = page.height - margins.bottom;
    let mut layout = Layout { fonts, pages: vec![Vec::new()], margins, width, bottom, y: margins.top };
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
        AbstractNumbering, BreakType, Docx, IndentLevel, Level, LevelJc, LevelText, NumberFormat, Numbering,
        NumberingId, PageMargin, Start, Style, StyleType, TableCell, TableRow,
    };

    fn para(text: &str) -> docx_rs::Paragraph {
        docx_rs::Paragraph::new().add_run(docx_rs::Run::new().add_text(text))
    }

    #[test]
    fn a_document_round_trips_to_outline_text_pages_and_pixels() {
        let level = |l: usize, format: &str, text: &str| {
            Level::new(l, Start::new(1), NumberFormat::new(format), LevelText::new(text), LevelJc::new("left"))
        };
        let item = |text: &str, l: usize| para(text).numbering(NumberingId::new(2), IndentLevel::new(l));
        let cell = |text: &str| TableCell::new().add_paragraph(para(text));
        let docx = Docx::new()
            .page_size(8000, 10000)
            .page_margin(PageMargin::new().top(1440).left(1440).right(1440).bottom(1440))
            .add_style(Style::new("Heading1", StyleType::Paragraph).name("heading 1"))
            .add_style(Style::new("Custom", StyleType::Paragraph).name("heading 2"))
            // docx-rs always writes its own abstract numbering 1.
            .add_abstract_numbering(
                AbstractNumbering::new(5).add_level(level(0, "decimal", "%1.")).add_level(level(1, "lowerLetter", "%1.%2)")),
            )
            .add_numbering(Numbering::new(2, 5))
            .add_paragraph(para("Intro").style("Heading1"))
            .add_paragraph(
                docx_rs::Paragraph::new()
                    .add_run(docx_rs::Run::new().add_text("plain "))
                    .add_run(docx_rs::Run::new().add_text("strong").bold())
                    .add_run(docx_rs::Run::new().add_text(" em").italic().underline("single").add_tab().add_text("x")),
            )
            .add_paragraph(item("first", 0))
            .add_paragraph(item("sub", 1))
            .add_paragraph(item("second", 0))
            .add_table(docx_rs::Table::new(vec![TableRow::new(vec![cell("a1"), cell("b1")])]))
            .add_paragraph(para("Details").style("Custom").page_break_before(true))
            .add_paragraph(docx_rs::Paragraph::new().add_run(docx_rs::Run::new().add_break(BreakType::Page)))
            .add_paragraph(para("Deep").style("Heading3"));
        let mut bytes = std::io::Cursor::new(Vec::new());
        docx.build().pack(&mut bytes).unwrap();
        let doc = DocxDoc::parse(bytes.get_ref()).unwrap();

        assert_eq!(doc.text(), "Intro\nplain strong em\tx\nfirst\nsub\nsecond\na1\tb1\nDetails\n\n\nDeep");
        let paragraphs: Vec<&Paragraph> =
            doc.outline.blocks.iter().filter_map(|b| if let Block::Paragraph(p) = b { Some(p) } else { None }).collect();
        let headings: Vec<_> = paragraphs.iter().filter_map(|p| Some((p.heading_level?, p.text.as_str()))).collect();
        assert_eq!(headings, [(1, "Intro"), (2, "Details"), (3, "Deep")]);
        let runs: Vec<_> = paragraphs[1].runs.iter().map(|r| (r.text.as_str(), r.bold, r.italic, r.underline)).collect();
        assert_eq!(runs, [("plain ", false, false, false), ("strong", true, false, false), (" em\tx", false, true, true)]);
        let markers: Vec<_> = paragraphs[2..5].iter().map(|p| p.list.as_ref().map(|l| (l.level, l.marker.as_str()))).collect();
        assert_eq!(markers, [Some((0, "1.")), Some((1, "1.a)")), Some((0, "2."))]);
        assert!(matches!(&doc.outline.blocks[5], Block::Table(t) if t.rows == [["a1", "b1"]]));

        assert_eq!((doc.page_count(), doc.page_size(0).unwrap()), (3, PageSize { width: 400.0, height: 500.0 }));
        let image = doc.render_page(0, 2.0, &Fonts::none()).unwrap();
        assert_eq!((image.width, image.height), (800, 1000));
        let dark = |x0: u32, y0: u32, x1, y1| (y0..y1).any(|y| (x0..x1).any(|x| image.pixel(x, y).is_some_and(|p| p[0] < 128)));
        // Text starts at the 72pt margins, scaled by 2.
        assert!(dark(144, 144, 344, 210) && !dark(0, 0, 800, 140) && !dark(0, 0, 140, 1000));
        assert!(matches!(doc.render_page(3, 1.0, &Fonts::none()), Err(DocsError::PageRange { index: 3, count: 3 })));
        assert!(matches!(DocxDoc::parse(b"not a zip"), Err(DocsError::Invalid { format: Format::Docx, .. })));
    }
}
