//! XLSX workbooks: cell queries for the agent, and a grid renderer with one page per worksheet.

use crate::docs::raster::{Canvas, Fonts, TextStyle};
use crate::docs::{DataQuery, DocsError, Format, PageSize, Paged, RenderOpts, Result, Rgba, json, page_at};
use calamine::{CellType, Data, Dimensions as Area, Range, Reader, SheetType, Xlsx};
use std::borrow::Cow;
use std::io::Cursor;

/// Rows and columns in an Excel worksheet.
const MAX_ROWS: u32 = 1_048_576;
const MAX_COLUMNS: u32 = 16_384;
/// Most cells one [`XlsxDoc::cells`] call returns.
pub const MAX_BLOCK_CELLS: u64 = 10_000;

/// Rows and columns drawn on a rendered page, counted from A1.
const RENDER_ROWS: u32 = 50;
const RENDER_COLUMNS: u32 = 20;
/// Grid geometry at scale 1, in points.
const COLUMN_WIDTH: f32 = 64.0;
const ROW_HEIGHT: f32 = 20.0;
const ROW_HEADER_WIDTH: f32 = 40.0;
const CELL_PADDING: f32 = 3.0;
/// Most `#` shown instead of a number too wide for its cell, as Excel does.
const MAX_HASHES: usize = 64;

/// Horizontal alignment as the fraction of the free cell width left of the text.
const LEFT: f32 = 0.0;
const CENTER: f32 = 0.5;
const RIGHT: f32 = 1.0;

/// A 0-based (row, column) cell position.
type Pos = (u32, u32);

/// A parsed XLSX workbook: every worksheet in workbook order with its values and formulas.
#[derive(Clone, Debug)]
pub struct XlsxDoc {
    sheets: Vec<Sheet>,
}

#[derive(Clone, Debug)]
struct Sheet {
    name: String,
    values: Range<Data>,
    /// Formula text without the leading `=`, empty where a cell has none.
    formulas: Range<String>,
    /// Bounding box of the non-empty values and formulas.
    used: Option<Area>,
}

/// The smallest area holding `area` and `p`.
fn include(area: Area, p: Pos) -> Area {
    Area::new((area.start.0.min(p.0), area.start.1.min(p.1)), (area.end.0.max(p.0), area.end.1.max(p.1)))
}

/// A1 notation: `B3` for one cell, `B3:D5` otherwise.
fn area_name(area: Area) -> String {
    let start = address(area.start);
    if area.start == area.end { start } else { format!("{start}:{}", address(area.end)) }
}

/// A worksheet as listed to the agent.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct SheetInfo {
    pub name: String,
    /// Size of the used range (the box around every non-empty cell).
    pub rows: u32,
    pub columns: u32,
    /// Top left cell of the used range, like `A1`; `None` for an empty sheet.
    pub start: Option<String>,
}

/// A cell value with its type. Serialized as `{"type": "float", "value": 1.5}`;
/// `empty` has no `value`.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Cell {
    Empty,
    String(String),
    Float(f64),
    Int(i64),
    Bool(bool),
    /// An error value such as `#DIV/0!`.
    Error(String),
    /// A date and/or time: the Excel serial number when stored, and ISO 8601.
    DateTime { serial: Option<f64>, iso: String },
    /// A time span: serial days when stored, and an ISO 8601 duration such as `PT1H30M0S`.
    Duration { serial: Option<f64>, iso: String },
}

impl Cell {
    fn from_data(data: &Data) -> Self {
        match data {
            Data::Empty => Self::Empty,
            Data::String(s) => Self::String(s.clone()),
            Data::Float(f) => Self::Float(*f),
            Data::Int(i) => Self::Int(*i),
            Data::Bool(b) => Self::Bool(*b),
            Data::Error(e) => Self::Error(e.to_string()),
            Data::DateTime(dt) if dt.is_duration() => {
                let (sign, h, m, s, ms) = hms((dt.as_f64() * 86_400_000.0).round() as i64);
                let frac = if ms == 0 { String::new() } else { format!(".{ms:03}") };
                Self::Duration { serial: Some(dt.as_f64()), iso: format!("{sign}PT{h}H{m}M{s}{frac}S") }
            }
            Data::DateTime(dt) => {
                let (y, mo, d, h, mi, s, ms) = dt.to_ymd_hms_milli();
                let frac = if ms == 0 { String::new() } else { format!(".{ms:03}") };
                let iso = format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}{frac}");
                Self::DateTime { serial: Some(dt.as_f64()), iso }
            }
            Data::DateTimeIso(s) => Self::DateTime { serial: None, iso: s.clone() },
            Data::DurationIso(s) => Self::Duration { serial: None, iso: s.clone() },
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct CellOut {
    /// Absolute A1 address, like `B3`.
    pub address: String,
    pub value: Cell,
    /// Formula without the leading `=`; `value` is its cached result.
    pub formula: Option<String>,
}

/// A rectangle of cells, row by row.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct CellBlock {
    pub sheet: String,
    /// The rectangle returned, in A1 notation.
    pub range: String,
    pub rows: Vec<Vec<CellOut>>,
}

impl XlsxDoc {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let invalid = |e: calamine::XlsxError| DocsError::invalid(Format::Xlsx, e);
        let mut book: Xlsx<Cursor<&[u8]>> = calamine::open_workbook_from_rs(Cursor::new(bytes)).map_err(invalid)?;
        let worksheets = book.sheets_metadata().iter().filter(|s| s.typ == SheetType::WorkSheet);
        let names: Vec<String> = worksheets.map(|s| s.name.clone()).collect();
        let mut sheets = Vec::with_capacity(names.len());
        for name in names {
            let values = book.worksheet_range(&name).map_err(invalid)?;
            let formulas = book.worksheet_formula(&name).map_err(invalid)?;
            let used = (used_cells(&values).chain(used_cells(&formulas)))
                .fold(None, |used: Option<Area>, p| Some(include(used.unwrap_or(Area::new(p, p)), p)));
            sheets.push(Sheet { name, values, formulas, used });
        }
        Ok(Self { sheets })
    }

    pub fn sheets(&self) -> Vec<SheetInfo> {
        let info = |s: &Sheet| {
            let (rows, columns) = s.used.map_or((0, 0), |u| (u.end.0 - u.start.0 + 1, u.end.1 - u.start.1 + 1));
            SheetInfo { name: s.name.clone(), rows, columns, start: s.used.map(|u| address(u.start)) }
        };
        self.sheets.iter().map(info).collect()
    }

    /// The cells of `range` (`B2:D5` or `C3`) on `sheet` (the first sheet when `None`), with absolute
    /// addresses. The bottom right corner is pulled in to the sheet's used range (never above or left
    /// of the top left corner), and at most [`MAX_BLOCK_CELLS`] cells are returned.
    pub fn cells(&self, sheet: Option<&str>, range: &str) -> Result<CellBlock> {
        let sheet = self.sheet(sheet)?;
        let mut area = parse_range(range)?;
        let (last_row, last_col) = sheet.used.map_or(area.start, |u| u.end);
        area.end = (area.end.0.min(last_row).max(area.start.0), area.end.1.min(last_col).max(area.start.1));
        if area.len() > MAX_BLOCK_CELLS {
            let (name, len) = (area_name(area), area.len());
            return Err(DocsError::Request(format!("range {name} has {len} cells, request at most {MAX_BLOCK_CELLS} at a time")));
        }
        let rows = (area.start.0..=area.end.0)
            .map(|row| {
                (area.start.1..=area.end.1)
                    .map(|col| CellOut {
                        address: address((row, col)),
                        value: Cell::from_data(sheet.value(row, col)),
                        formula: sheet.formula(row, col).map(str::to_owned),
                    })
                    .collect()
            })
            .collect();
        Ok(CellBlock { sheet: sheet.name.clone(), range: area_name(area), rows })
    }

    /// The sheet called `name` (exact match first, then ignoring case), or the first sheet when `None`.
    fn sheet(&self, name: Option<&str>) -> Result<&Sheet> {
        let Some(name) = name else {
            return self.sheets.first().ok_or_else(|| DocsError::Request("the workbook has no sheets".into()));
        };
        let lower = name.to_lowercase();
        let exact = self.sheets.iter().find(|s| s.name == name);
        exact.or_else(|| self.sheets.iter().find(|s| s.name.to_lowercase() == lower)).ok_or_else(|| {
            let names: Vec<&str> = self.sheets.iter().map(|s| s.name.as_str()).collect();
            DocsError::Request(format!("no sheet named `{name}`, the sheets are: {}", names.join(", ")))
        })
    }
}

impl Paged for XlsxDoc {
    fn page_count(&self) -> usize {
        self.sheets.len()
    }

    fn page_size(&self, index: usize) -> Result<PageSize> {
        let (rows, cols) = page_at(&self.sheets, index)?.render_extent();
        Ok(PageSize { width: ROW_HEADER_WIDTH + cols as f32 * COLUMN_WIDTH, height: (rows + 1) as f32 * ROW_HEIGHT })
    }

    fn query(&self, query: &DataQuery) -> Option<Result<serde_json::Value>> {
        match query {
            DataQuery::Sheets => Some(json(&self.sheets())),
            DataQuery::Cells { sheet, range } => Some(self.cells(sheet.as_deref(), range).and_then(|c| json(&c))),
            _ => None,
        }
    }

    fn render(&self, index: usize, scale: f32, fonts: &Fonts, opts: &RenderOpts) -> Result<Rgba> {
        let (size, sheet) = (self.page_size(index)?, &self.sheets[index]);
        let (rows, cols) = sheet.render_extent();
        let (width, height) = size.pixels(scale);
        let (w, h) = (width as f32, height as f32);
        let (col_w, row_h, head_w, pad) =
            (COLUMN_WIDTH * scale, ROW_HEIGHT * scale, ROW_HEADER_WIDTH * scale, CELL_PADDING * scale);
        let (palette, style) = (&opts.palette, opts.text(opts.typography.cell * scale));
        let header_style = TextStyle { color: palette.header_text, ..style };
        let text_top = (row_h - fonts.line_height(style.size)) / 2.0;
        let width_of = |text: &str| fonts.text_width(text, style.size, false);
        let mut canvas = Canvas::new(width, height, palette.page);

        canvas.fill_rect(0.0, 0.0, w, row_h, palette.header);
        canvas.fill_rect(0.0, 0.0, head_w, h, palette.header);
        for c in 1..=cols {
            canvas.fill_rect((head_w + c as f32 * col_w).min(w - 1.0), 0.0, 1.0, h, palette.grid);
        }
        for r in 1..=rows {
            canvas.fill_rect(0.0, ((r + 1) as f32 * row_h).min(h - 1.0), w, 1.0, palette.grid);
        }
        canvas.fill_rect(head_w, 0.0, 1.0, h, palette.rule);
        canvas.fill_rect(0.0, row_h, w, 1.0, palette.rule);

        for c in 0..cols {
            let name = column_name(c);
            let x = head_w + c as f32 * col_w + (col_w - width_of(&name)) / 2.0;
            canvas.draw_text(fonts, header_style, x, text_top, &name);
        }
        let avail = col_w - 2.0 * pad;
        for r in 0..rows {
            let y = (r + 1) as f32 * row_h + text_top;
            let number = (u64::from(r) + 1).to_string();
            canvas.draw_text(fonts, header_style, (head_w - width_of(&number)) / 2.0, y, &number);
            for c in 0..cols {
                let (text, align) = display(sheet.value(r, c));
                let text = if align == RIGHT && width_of(&text) > avail {
                    Cow::Owned("#".repeat(((avail / width_of("#").max(1.0)) as usize).min(MAX_HASHES)))
                } else {
                    Cow::Borrowed(fit(text.split('\n').next().unwrap_or(""), avail, width_of))
                };
                let x = head_w + c as f32 * col_w + pad + (avail - width_of(&text)) * align;
                canvas.draw_text(fonts, style, x, y, &text);
            }
        }
        Ok(canvas.into_rgba())
    }
}

impl Sheet {
    fn value(&self, row: u32, col: u32) -> &Data {
        self.values.get_value((row, col)).unwrap_or(&Data::Empty)
    }

    fn formula(&self, row: u32, col: u32) -> Option<&str> {
        self.formulas.get_value((row, col)).map(String::as_str).filter(|f| !f.is_empty())
    }

    /// Rows and columns drawn, from A1 to the end of the used range, capped.
    fn render_extent(&self) -> (u32, u32) {
        self.used.map_or((1, 1), |u| ((u.end.0 + 1).min(RENDER_ROWS), (u.end.1 + 1).min(RENDER_COLUMNS)))
    }
}

/// Absolute positions of the non-empty cells of `range`.
fn used_cells<T: CellType>(range: &Range<T>) -> impl Iterator<Item = Pos> + '_ {
    let (row0, col0) = range.start().unwrap_or_default();
    // The range spans at most the sheet's u32 rows and columns, so relative indexes fit in u32.
    range.used_cells().map(move |(r, c, _)| (row0 + r as u32, col0 + c as u32))
}

/// Parses `B2:D5` or `C3` into a normalized area.
fn parse_range(range: &str) -> Result<Area> {
    let (a, b) = range.split_once(':').unwrap_or((range, range));
    let a = parse_cell(a)?;
    Ok(include(Area::new(a, a), parse_cell(b)?))
}

/// Parses an A1 cell reference (`$` markers allowed, case insensitive).
fn parse_cell(text: &str) -> Result<Pos> {
    let s = text.trim();
    let s = s.strip_prefix('$').unwrap_or(s);
    let (letters, digits) = s.split_at(s.bytes().take_while(u8::is_ascii_alphabetic).count());
    let digits = digits.strip_prefix('$').unwrap_or(digits);
    let col = letters
        .bytes()
        .fold(0u32, |acc, b| acc.saturating_mul(26).saturating_add(u32::from(b.to_ascii_uppercase() - b'A') + 1));
    let row = digits.parse::<u32>().ok().filter(|_| digits.bytes().all(|b| b.is_ascii_digit()));
    match row {
        Some(row @ 1..=MAX_ROWS) if (1..=MAX_COLUMNS).contains(&col) => Ok((row - 1, col - 1)),
        _ => Err(DocsError::Request(format!("`{text}` is not a cell in A1 notation such as B3"))),
    }
}

fn address((row, col): Pos) -> String {
    format!("{}{}", column_name(col), u64::from(row) + 1)
}

/// The letters of 0-based column `col` (`A`, `Z`, `AA`, ...).
fn column_name(col: u32) -> String {
    let letter = char::from(b'A' + (col % 26) as u8);
    if col < 26 { letter.to_string() } else { format!("{}{letter}", column_name(col / 26 - 1)) }
}

/// Sign, hours, minutes, seconds and milliseconds of a span of `ms` milliseconds.
fn hms(ms: i64) -> (&'static str, u64, u64, u64, u64) {
    let abs = ms.unsigned_abs();
    (if ms < 0 { "-" } else { "" }, abs / 3_600_000, abs / 60_000 % 60, abs / 1000 % 60, abs % 1000)
}

/// The text a cell shows and its alignment, Excel style: numbers and dates right, booleans and
/// errors centered, text left.
fn display(data: &Data) -> (Cow<'_, str>, f32) {
    match data {
        Data::Empty => ("".into(), LEFT),
        Data::String(s) => (s.into(), LEFT),
        Data::DateTimeIso(s) | Data::DurationIso(s) => (s.into(), RIGHT),
        Data::Bool(b) => ((if *b { "TRUE" } else { "FALSE" }).into(), CENTER),
        Data::Error(e) => (e.to_string().into(), CENTER),
        Data::Int(i) => (i.to_string().into(), RIGHT),
        Data::Float(f) => (general(*f).into(), RIGHT),
        Data::DateTime(dt) if dt.is_duration() => {
            let (sign, h, m, s, _) = hms(((dt.as_f64() * 86_400.0).round() as i64).saturating_mul(1000));
            (format!("{sign}{h}:{m:02}:{s:02}").into(), RIGHT)
        }
        Data::DateTime(dt) => {
            let (y, mo, d, h, mi, s, _) = dt.to_ymd_hms_milli();
            let text = match dt.as_f64() {
                serial if serial < 1.0 => format!("{h:02}:{mi:02}:{s:02}"),
                serial if serial.fract() == 0.0 => format!("{y:04}-{mo:02}-{d:02}"),
                _ => format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}"),
            };
            (text.into(), RIGHT)
        }
    }
}

/// A number the way the General format shows it: integers whole, very large or small magnitudes
/// in scientific notation, otherwise up to nine decimals.
fn general(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.0}")
    } else if v.is_finite() && v != 0.0 && (v.abs() >= 1e15 || v.abs() < 1e-4) {
        format!("{v:.4e}")
    } else {
        let text = format!("{v:.9}");
        if text.contains('.') { text.trim_end_matches('0').trim_end_matches('.').to_owned() } else { text }
    }
}

/// The longest prefix of `text` no wider than `max` pixels.
fn fit(text: &str, max: f32, width_of: impl Fn(&str) -> f32) -> &str {
    if width_of(text) <= max {
        return text;
    }
    let mut width = 0.0;
    let end = text.char_indices().find(|&(i, ch)| {
        width += width_of(&text[i..i + ch.len_utf8()]);
        width > max
    });
    &text[..end.map_or(text.len(), |(i, _)| i)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_xlsxwriter::{Format as CellFormat, Formula, Workbook};

    #[test]
    fn a_workbook_round_trips_to_sheets_cells_and_a_grid() {
        // `Data` from A1 with text, numbers, a bool, a formula and a date; `Offset` starting at D3.
        let mut book = Workbook::new();
        let data = book.add_worksheet().set_name("Data").unwrap();
        data.write_string(0, 0, "Name").unwrap();
        data.write_string(1, 0, "apple").unwrap();
        data.write_number(1, 1, 3.0).unwrap();
        data.write_boolean(2, 0, true).unwrap();
        data.write_formula(2, 1, Formula::new("=SUM(B2:B2)").set_result("3")).unwrap();
        data.write_number_with_format(3, 0, 45366.0, &CellFormat::new().set_num_format("yyyy-mm-dd")).unwrap();
        let offset = book.add_worksheet().set_name("Offset").unwrap();
        offset.write_string(2, 3, "origin").unwrap();
        offset.write_number(3, 4, 42.0).unwrap();
        let doc = XlsxDoc::parse(&book.save_to_buffer().unwrap()).unwrap();

        let info = |name: &str, rows, columns, start: &str| SheetInfo { name: name.into(), rows, columns, start: Some(start.into()) };
        assert_eq!(doc.sheets(), [info("Data", 4, 2, "A1"), info("Offset", 2, 2, "D3")]);
        let block = doc.cells(None, "A1:B4").unwrap();
        let values: Vec<_> = block.rows.iter().flatten().map(|c| (c.address.as_str(), c.value.clone(), c.formula.as_deref())).collect();
        let date = Cell::DateTime { serial: Some(45366.0), iso: "2024-03-15T00:00:00".into() };
        assert_eq!(
            values,
            [
                ("A1", Cell::String("Name".into()), None),
                ("B1", Cell::Empty, None),
                ("A2", Cell::String("apple".into()), None),
                ("B2", Cell::Float(3.0), None),
                ("A3", Cell::Bool(true), None),
                ("B3", Cell::Float(3.0), Some("SUM(B2:B2)")),
                ("A4", date, None),
                ("B4", Cell::Empty, None),
            ]
        );
        // Ranges are absolute even when the data starts elsewhere, and clipped to the used area.
        assert_eq!(doc.cells(Some("offset"), "$E$4").unwrap().rows[0][0].value, Cell::Float(42.0));
        let wide = doc.cells(Some("Offset"), "A1:Z1000").unwrap();
        assert_eq!((wide.range.as_str(), &wide.rows[2][3].value), ("A1:E4", &Cell::String("origin".into())));
        for (sheet, range) in [(Some("Missing"), "A1"), (None, "B"), (None, "12"), (None, "A0"), (None, "XFE1")] {
            assert!(matches!(doc.cells(sheet, range), Err(DocsError::Request(_))), "{sheet:?} {range}");
        }
        assert_eq!([0, 25, 26, 701, 702].map(column_name), ["A", "Z", "AA", "ZZ", "AAA"]);

        let size = PageSize { width: ROW_HEADER_WIDTH + 5.0 * COLUMN_WIDTH, height: 5.0 * ROW_HEIGHT };
        assert_eq!(doc.page_size(1).unwrap(), size);
        let image = doc.render_page(1, 1.0, &Fonts::none()).unwrap();
        let dark = |x: u32, y: u32| (y..y + 18).any(|py| (x..x + 62).any(|px| image.pixel(px, py).is_some_and(|p| p[0] < 128)));
        // Inside cell A1, empty, and D3, which holds `origin`.
        assert!(!dark(41, 21) && dark(233, 61));
        assert_ne!(image.pixel(ROW_HEADER_WIDTH as u32 + 3, 3), Some([255; 4]));
        assert!(matches!(doc.render_page(2, 1.0, &Fonts::none()), Err(DocsError::PageRange { index: 2, count: 2 })));
        assert!(matches!(XlsxDoc::parse(b"PK\x03\x04 not a workbook"), Err(DocsError::Invalid { .. })));
    }
}
