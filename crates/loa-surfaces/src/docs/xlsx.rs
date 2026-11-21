//! XLSX workbooks: cell queries for the agent, and a grid renderer with one
//! page per worksheet. Reading goes through `calamine`.

use crate::docs::raster::{Canvas, Fonts};
use crate::docs::{DataQuery, DocsError, Format, PageSize, Paged, RenderOpts, Result, Rgba, json};
use calamine::{Data, ExcelDateTime, Range, Reader, SheetType, Xlsx};
use std::fmt::Write as _;
use std::io::Cursor;

/// Rows in an Excel worksheet.
const MAX_ROWS: u32 = 1_048_576;
/// Columns in an Excel worksheet.
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
/// Shown instead of a number too wide for its cell, as Excel does.
const HASHES: &str = "################################################################";

/// Stand in for cells outside a sheet's stored range.
static EMPTY: Data = Data::Empty;

/// A parsed XLSX workbook: every worksheet in workbook order with its values
/// and formulas.
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

/// A rectangle of cells, 0-based and inclusive.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Area {
    first_row: u32,
    first_col: u32,
    last_row: u32,
    last_col: u32,
}

impl Area {
    fn cell(row: u32, col: u32) -> Self {
        Self { first_row: row, first_col: col, last_row: row, last_col: col }
    }

    fn include(area: Option<Self>, row: u32, col: u32) -> Self {
        match area {
            None => Self::cell(row, col),
            Some(a) => Self {
                first_row: a.first_row.min(row),
                first_col: a.first_col.min(col),
                last_row: a.last_row.max(row),
                last_col: a.last_col.max(col),
            },
        }
    }

    fn rows(self) -> u32 {
        self.last_row - self.first_row + 1
    }

    fn columns(self) -> u32 {
        self.last_col - self.first_col + 1
    }

    /// A1 notation: `B3` for one cell, `B3:D5` otherwise.
    fn name(self) -> String {
        let mut out = address(self.first_row, self.first_col);
        if self.last_row != self.first_row || self.last_col != self.first_col {
            out.push(':');
            push_address(&mut out, self.last_row, self.last_col);
        }
        out
    }
}

/// A worksheet as listed to the agent.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct SheetInfo {
    pub name: String,
    /// Height of the used range (the box around every non-empty cell).
    pub rows: u32,
    /// Width of the used range.
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
    /// A date and/or time. `serial` is the Excel serial number in the
    /// workbook's epoch when the file stores one; `iso` is ISO 8601.
    DateTime { serial: Option<f64>, iso: String },
    /// A time span. `serial` is in days when the file stores one; `iso` is an
    /// ISO 8601 duration such as `PT1H30M0S`.
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
            Data::DateTime(dt) => {
                let mut iso = String::new();
                if dt.is_duration() {
                    push_iso_duration(&mut iso, dt.as_f64());
                    Self::Duration { serial: Some(dt.as_f64()), iso }
                } else {
                    push_iso_datetime(&mut iso, dt);
                    Self::DateTime { serial: Some(dt.as_f64()), iso }
                }
            }
            Data::DateTimeIso(s) => Self::DateTime { serial: None, iso: s.clone() },
            Data::DurationIso(s) => Self::Duration { serial: None, iso: s.clone() },
        }
    }
}

/// One cell of a [`CellBlock`].
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

pub(crate) fn open(_: Format, bytes: &[u8]) -> Result<Box<dyn Paged>> {
    Ok(Box::new(XlsxDoc::parse(bytes)?))
}

impl XlsxDoc {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let invalid = |e: calamine::XlsxError| DocsError::invalid(Format::Xlsx, e);
        let mut book: Xlsx<Cursor<&[u8]>> =
            calamine::open_workbook_from_rs(Cursor::new(bytes)).map_err(invalid)?;
        let names: Vec<String> = book
            .sheets_metadata()
            .iter()
            .filter(|s| s.typ == SheetType::WorkSheet)
            .map(|s| s.name.clone())
            .collect();
        let mut sheets = Vec::with_capacity(names.len());
        for name in names {
            let values = book.worksheet_range(&name).map_err(invalid)?;
            let formulas = book.worksheet_formula(&name).map_err(invalid)?;
            let mut used = None;
            for_used_cells(&values, |row, col, _| used = Some(Area::include(used, row, col)));
            for_used_cells(&formulas, |row, col, _| used = Some(Area::include(used, row, col)));
            sheets.push(Sheet { name, values, formulas, used });
        }
        Ok(Self { sheets })
    }

    /// The worksheets in workbook order.
    pub fn sheets(&self) -> Vec<SheetInfo> {
        self.sheets
            .iter()
            .map(|s| SheetInfo {
                name: s.name.clone(),
                rows: s.used.map_or(0, Area::rows),
                columns: s.used.map_or(0, Area::columns),
                start: s.used.map(|u| address(u.first_row, u.first_col)),
            })
            .collect()
    }

    /// The cells of `range` (A1 notation, `B2:D5` or `C3`) on `sheet` (the
    /// first sheet when `None`). Addresses are absolute. The bottom right
    /// corner is pulled in to the sheet's used range (never above or left of
    /// the top left corner), and at most [`MAX_BLOCK_CELLS`] cells are
    /// returned.
    pub fn cells(&self, sheet: Option<&str>, range: &str) -> Result<CellBlock> {
        let sheet = &self.sheets[self.sheet_index(sheet)?];
        let mut area = parse_range(range)?;
        if let Some(used) = sheet.used {
            area.last_row = area.last_row.min(used.last_row).max(area.first_row);
            area.last_col = area.last_col.min(used.last_col).max(area.first_col);
        } else {
            area.last_row = area.first_row;
            area.last_col = area.first_col;
        }
        let count = u64::from(area.rows()) * u64::from(area.columns());
        if count > MAX_BLOCK_CELLS {
            return Err(DocsError::Request(format!(
                "range {} has {count} cells, request at most {MAX_BLOCK_CELLS} at a time",
                area.name()
            )));
        }
        let rows = (area.first_row..=area.last_row)
            .map(|row| {
                (area.first_col..=area.last_col)
                    .map(|col| CellOut {
                        address: address(row, col),
                        value: Cell::from_data(sheet.value(row, col)),
                        formula: sheet.formula(row, col).map(str::to_owned),
                    })
                    .collect()
            })
            .collect();
        Ok(CellBlock { sheet: sheet.name.clone(), range: area.name(), rows })
    }

    fn sheet_index(&self, name: Option<&str>) -> Result<usize> {
        let names: Vec<&str> = self.sheets.iter().map(|s| s.name.as_str()).collect();
        find_sheet(&names, name)
    }

    fn page(&self, index: usize) -> Result<&Sheet> {
        self.sheets.get(index).ok_or_else(|| DocsError::page_range(index, self.sheets.len()))
    }
}

impl Paged for XlsxDoc {
    fn page_count(&self) -> usize {
        self.sheets.len()
    }

    fn page_size(&self, index: usize) -> Result<PageSize> {
        let (rows, cols) = self.page(index)?.render_extent();
        Ok(PageSize {
            width: ROW_HEADER_WIDTH + cols as f32 * COLUMN_WIDTH,
            height: (rows + 1) as f32 * ROW_HEIGHT,
        })
    }

    fn query(&self, query: &DataQuery) -> Option<Result<serde_json::Value>> {
        match query {
            DataQuery::Sheets => Some(json(&self.sheets())),
            DataQuery::Cells { sheet, range } => Some(self.cells(sheet.as_deref(), range).and_then(|c| json(&c))),
            _ => None,
        }
    }

    fn render(&self, index: usize, scale: f32, fonts: &Fonts, opts: &RenderOpts) -> Result<Rgba> {
        let size = self.page_size(index)?;
        let sheet = self.page(index)?;
        let (rows, cols) = sheet.render_extent();
        let (width, height) = size.pixels(scale);
        let (w, h) = (width as f32, height as f32);
        let col_w = COLUMN_WIDTH * scale;
        let row_h = ROW_HEIGHT * scale;
        let head_w = ROW_HEADER_WIDTH * scale;
        let pad = CELL_PADDING * scale;
        let (palette, style) = (&opts.palette, opts.text(opts.typography.cell * scale));
        let header_style = style.color(palette.header_text);
        let text_top = (row_h - fonts.line_height(style.size)) / 2.0;
        let mut canvas = Canvas::new(width, height, palette.page);

        canvas.fill_rect(0.0, 0.0, w, row_h, palette.header);
        canvas.fill_rect(0.0, 0.0, head_w, h, palette.header);
        for c in 1..=cols {
            let x = (head_w + c as f32 * col_w).min(w - 1.0);
            canvas.fill_rect(x, 0.0, 1.0, h, palette.grid);
        }
        for r in 1..=rows {
            let y = ((r + 1) as f32 * row_h).min(h - 1.0);
            canvas.fill_rect(0.0, y, w, 1.0, palette.grid);
        }
        canvas.fill_rect(head_w, 0.0, 1.0, h, palette.rule);
        canvas.fill_rect(0.0, row_h, w, 1.0, palette.rule);

        let mut buf = String::new();
        for c in 0..cols {
            buf.clear();
            push_column_name(&mut buf, c);
            let x = head_w + c as f32 * col_w;
            let tw = fonts.text_width(&buf, style.size, false);
            canvas.draw_text(fonts, header_style, x + (col_w - tw) / 2.0, text_top, &buf);
        }
        for r in 0..rows {
            buf.clear();
            let _ = write!(buf, "{}", u64::from(r) + 1); // writing to a String cannot fail
            let y = (r + 1) as f32 * row_h;
            let tw = fonts.text_width(&buf, style.size, false);
            canvas.draw_text(fonts, header_style, (head_w - tw) / 2.0, y + text_top, &buf);
        }

        let avail = col_w - 2.0 * pad;
        for r in 0..rows {
            let y = (r + 1) as f32 * row_h + text_top;
            for c in 0..cols {
                let data = sheet.value(r, c);
                if matches!(data, Data::Empty) {
                    continue;
                }
                let (text, align) = display(data, &mut buf);
                let text = match align {
                    Align::Right if fonts.text_width(text, style.size, false) > avail => {
                        let hash = fonts.text_width("#", style.size, false).max(1.0);
                        &HASHES[..((avail / hash) as usize).min(HASHES.len())]
                    }
                    _ => fit(fonts, text.split('\n').next().unwrap_or(""), style.size, avail),
                };
                let tw = fonts.text_width(text, style.size, false);
                let left = head_w + c as f32 * col_w;
                let x = match align {
                    Align::Left => left + pad,
                    Align::Right => left + col_w - pad - tw,
                    Align::Center => left + (col_w - tw) / 2.0,
                };
                canvas.draw_text(fonts, style, x, y, text);
            }
        }
        Ok(canvas.into_rgba())
    }
}

impl Sheet {
    fn value(&self, row: u32, col: u32) -> &Data {
        self.values.get_value((row, col)).unwrap_or(&EMPTY)
    }

    fn formula(&self, row: u32, col: u32) -> Option<&str> {
        self.formulas.get_value((row, col)).map(String::as_str).filter(|f| !f.is_empty())
    }

    /// Rows and columns drawn, from A1 to the end of the used range, capped.
    fn render_extent(&self) -> (u32, u32) {
        self.used.map_or((1, 1), |u| {
            ((u.last_row + 1).min(RENDER_ROWS), (u.last_col + 1).min(RENDER_COLUMNS))
        })
    }

}

/// Calls `f` with the absolute row, column and value of each non-empty cell.
fn for_used_cells<T: calamine::CellType>(range: &Range<T>, mut f: impl FnMut(u32, u32, &T)) {
    let Some((row0, col0)) = range.start() else { return };
    for (r, c, value) in range.used_cells() {
        // The range spans at most the sheet's u32 rows and columns, so the
        // relative indexes fit in u32.
        f(row0 + r as u32, col0 + c as u32, value);
    }
}

/// Index of the sheet called `name` (exact match first, then ignoring case),
/// or of the first sheet when `None`.
fn find_sheet(names: &[&str], name: Option<&str>) -> Result<usize> {
    match name {
        None if names.is_empty() => Err(DocsError::Request("the workbook has no sheets".into())),
        None => Ok(0),
        Some(name) => names
            .iter()
            .position(|n| *n == name)
            .or_else(|| {
                let lower = name.to_lowercase();
                names.iter().position(|n| n.to_lowercase() == lower)
            })
            .ok_or_else(|| {
                DocsError::Request(format!("no sheet named `{name}`, the sheets are: {}", names.join(", ")))
            }),
    }
}

/// Parses `B2:D5` or `C3` into a normalized area.
fn parse_range(range: &str) -> Result<Area> {
    let (a, b) = match range.split_once(':') {
        Some((a, b)) => (parse_cell(a)?, parse_cell(b)?),
        None => {
            let cell = parse_cell(range)?;
            (cell, cell)
        }
    };
    Ok(Area {
        first_row: a.0.min(b.0),
        first_col: a.1.min(b.1),
        last_row: a.0.max(b.0),
        last_col: a.1.max(b.1),
    })
}

/// Parses an A1 cell reference (`$` markers allowed, case insensitive) into
/// a 0-based (row, column).
fn parse_cell(text: &str) -> Result<(u32, u32)> {
    let bad = || DocsError::Request(format!("`{text}` is not a cell in A1 notation such as B3"));
    let s = text.trim();
    let s = s.strip_prefix('$').unwrap_or(s);
    let letters = s.bytes().take_while(u8::is_ascii_alphabetic).count();
    if letters == 0 || letters > 3 {
        return Err(bad());
    }
    let (col_part, rest) = s.split_at(letters);
    let rest = rest.strip_prefix('$').unwrap_or(rest);
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let col = col_part.bytes().fold(0u32, |acc, b| acc * 26 + u32::from(b.to_ascii_uppercase() - b'A') + 1);
    let row: u32 = rest.parse().map_err(|_| bad())?;
    if col > MAX_COLUMNS || row == 0 || row > MAX_ROWS {
        return Err(bad());
    }
    Ok((row - 1, col - 1))
}

fn address(row: u32, col: u32) -> String {
    let mut out = String::new();
    push_address(&mut out, row, col);
    out
}

fn push_address(out: &mut String, row: u32, col: u32) {
    push_column_name(out, col);
    let _ = write!(out, "{}", u64::from(row) + 1); // writing to a String cannot fail
}

/// Appends the letters of 0-based column `col` (`A`, `Z`, `AA`, ...).
fn push_column_name(out: &mut String, col: u32) {
    // 26^7 exceeds u32::MAX, so seven letters always suffice.
    let mut letters = [0u8; 7];
    let mut len = 0;
    let mut n = u64::from(col) + 1;
    while n > 0 {
        letters[len] = b'A' + ((n - 1) % 26) as u8;
        len += 1;
        n = (n - 1) / 26;
    }
    out.extend(letters[..len].iter().rev().map(|&b| char::from(b)));
}

/// `YYYY-MM-DDTHH:MM:SS`, with `.mmm` when there are milliseconds.
fn push_iso_datetime(out: &mut String, dt: &ExcelDateTime) {
    let (y, mo, d, h, mi, s, ms) = dt.to_ymd_hms_milli();
    let _ = write!(out, "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}"); // writing to a String cannot fail
    if ms != 0 {
        let _ = write!(out, ".{ms:03}");
    }
}

/// ISO 8601 duration of `days`, like `PT36H5M0S`.
fn push_iso_duration(out: &mut String, days: f64) {
    let ms = (days * 86_400_000.0).round() as i64;
    let abs = ms.unsigned_abs();
    let sign = if ms < 0 { "-" } else { "" };
    let (h, m, s, frac) = (abs / 3_600_000, abs / 60_000 % 60, abs / 1000 % 60, abs % 1000);
    // writing to a String cannot fail
    let _ = if frac == 0 {
        write!(out, "{sign}PT{h}H{m}M{s}S")
    } else {
        write!(out, "{sign}PT{h}H{m}M{s}.{frac:03}S")
    };
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Align {
    Left,
    Right,
    Center,
}

/// The text a cell shows and how it aligns, Excel style: numbers and dates
/// right, booleans and errors centered, text left.
fn display<'a>(data: &'a Data, buf: &'a mut String) -> (&'a str, Align) {
    buf.clear();
    // Writing to a String cannot fail.
    match data {
        Data::Empty => ("", Align::Left),
        Data::String(s) => (s.as_str(), Align::Left),
        Data::DateTimeIso(s) | Data::DurationIso(s) => (s.as_str(), Align::Right),
        Data::Bool(b) => (if *b { "TRUE" } else { "FALSE" }, Align::Center),
        Data::Error(e) => {
            let _ = write!(buf, "{e}");
            (buf.as_str(), Align::Center)
        }
        Data::Int(i) => {
            let _ = write!(buf, "{i}");
            (buf.as_str(), Align::Right)
        }
        Data::Float(f) => {
            push_number(buf, *f);
            (buf.as_str(), Align::Right)
        }
        Data::DateTime(dt) if dt.is_duration() => {
            let secs = (dt.as_f64() * 86_400.0).round() as i64;
            let sign = if secs < 0 { "-" } else { "" };
            let abs = secs.unsigned_abs();
            let _ = write!(buf, "{sign}{}:{:02}:{:02}", abs / 3600, abs / 60 % 60, abs % 60);
            (buf.as_str(), Align::Right)
        }
        Data::DateTime(dt) => {
            let (y, mo, d, h, mi, s, _) = dt.to_ymd_hms_milli();
            let serial = dt.as_f64();
            let _ = if serial < 1.0 {
                write!(buf, "{h:02}:{mi:02}:{s:02}")
            } else if serial.fract() == 0.0 {
                write!(buf, "{y:04}-{mo:02}-{d:02}")
            } else {
                write!(buf, "{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
            };
            (buf.as_str(), Align::Right)
        }
    }
}

/// A number the way the General format shows it: integers whole, very large
/// or small magnitudes in scientific notation, otherwise up to nine decimals.
fn push_number(buf: &mut String, v: f64) {
    // Writing to a String cannot fail.
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        let _ = write!(buf, "{v:.0}");
    } else if v.is_finite() && v != 0.0 && (v.abs() >= 1e15 || v.abs() < 1e-4) {
        let _ = write!(buf, "{v:.4e}");
    } else {
        let _ = write!(buf, "{v:.9}");
        if buf.contains('.') {
            let len = buf.trim_end_matches('0').trim_end_matches('.').len();
            buf.truncate(len);
        }
    }
}

/// The longest prefix of `text` that fits in `max` pixels.
fn fit<'t>(fonts: &Fonts, text: &'t str, size: f32, max: f32) -> &'t str {
    if fonts.text_width(text, size, false) <= max {
        return text;
    }
    let mut width = 0.0;
    for (i, ch) in text.char_indices() {
        width += fonts.text_width(&text[i..i + ch.len_utf8()], size, false);
        if width > max {
            return &text[..i];
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_xlsxwriter::{Format as CellFormat, Formula, Workbook};

    /// Two sheets: `Data` from A1 with text, numbers, a bool, a formula and a
    /// date; `Offset` whose data starts at D3.
    fn workbook() -> Vec<u8> {
        let mut book = Workbook::new();
        let data = book.add_worksheet();
        data.set_name("Data").unwrap();
        data.write_string(0, 0, "Name").unwrap();
        data.write_string(0, 1, "Qty").unwrap();
        data.write_string(1, 0, "apple").unwrap();
        data.write_number(1, 1, 3.0).unwrap();
        data.write_string(2, 0, "pear").unwrap();
        data.write_number(2, 1, 4.5).unwrap();
        data.write_boolean(3, 0, true).unwrap();
        data.write_formula(3, 1, Formula::new("=SUM(B2:B3)").set_result("7.5")).unwrap();
        let date = CellFormat::new().set_num_format("yyyy-mm-dd");
        data.write_number_with_format(4, 0, 45366.0, &date).unwrap();
        let offset = book.add_worksheet();
        offset.set_name("Offset").unwrap();
        offset.write_string(2, 3, "origin").unwrap();
        offset.write_number(3, 4, 42.0).unwrap();
        book.save_to_buffer().unwrap()
    }

    fn cell(doc: &XlsxDoc, sheet: &str, at: &str) -> CellOut {
        let block = doc.cells(Some(sheet), at).unwrap();
        block.rows[0][0].clone()
    }

    #[test]
    fn reads_sheets_values_formulas_and_dates() {
        let doc = XlsxDoc::parse(&workbook()).unwrap();
        assert_eq!(
            doc.sheets(),
            [
                SheetInfo { name: "Data".into(), rows: 5, columns: 2, start: Some("A1".into()) },
                SheetInfo { name: "Offset".into(), rows: 2, columns: 2, start: Some("D3".into()) },
            ]
        );
        let block = doc.cells(None, "A1:B4").unwrap();
        assert_eq!(block.sheet, "Data");
        assert_eq!(block.range, "A1:B4");
        let values: Vec<Vec<Cell>> =
            block.rows.iter().map(|r| r.iter().map(|c| c.value.clone()).collect()).collect();
        assert_eq!(
            values,
            [
                [Cell::String("Name".into()), Cell::String("Qty".into())],
                [Cell::String("apple".into()), Cell::Float(3.0)],
                [Cell::String("pear".into()), Cell::Float(4.5)],
                [Cell::Bool(true), Cell::Float(7.5)],
            ]
        );
        let total = &block.rows[3][1];
        assert_eq!(total.address, "B4");
        assert_eq!(total.formula.as_deref(), Some("SUM(B2:B3)"));
        assert_eq!(block.rows[1][1].formula, None);
        assert_eq!(
            cell(&doc, "Data", "a5").value,
            Cell::DateTime { serial: Some(45366.0), iso: "2024-03-15T00:00:00".into() }
        );
    }

    #[test]
    fn ranges_address_cells_absolutely_when_data_does_not_start_at_a1() {
        let doc = XlsxDoc::parse(&workbook()).unwrap();
        let block = doc.cells(Some("Offset"), "C2:E4").unwrap();
        assert_eq!(block.range, "C2:E4");
        assert_eq!(block.rows.len(), 3);
        assert!(block.rows.iter().all(|r| r.len() == 3));
        assert_eq!(block.rows[0][0].address, "C2");
        assert_eq!(block.rows[0][0].value, Cell::Empty);
        assert_eq!(block.rows[1][1].address, "D3");
        assert_eq!(block.rows[1][1].value, Cell::String("origin".into()));
        assert_eq!(block.rows[2][2].address, "E4");
        assert_eq!(block.rows[2][2].value, Cell::Float(42.0));

        assert_eq!(cell(&doc, "Offset", "$E$4").value, Cell::Float(42.0));
        assert_eq!(cell(&doc, "Offset", "A1").value, Cell::Empty);

        let wide = doc.cells(Some("Offset"), "A1:Z1000").unwrap();
        assert_eq!(wide.range, "A1:E4");
        assert_eq!(wide.rows[2][3].value, Cell::String("origin".into()));
    }

    #[test]
    fn unknown_sheets_and_bad_ranges_are_request_errors() {
        let doc = XlsxDoc::parse(&workbook()).unwrap();
        for (sheet, range) in [(Some("Missing"), "A1"), (None, "B"), (None, "12"), (None, "A0"), (None, "XFE1")] {
            let err = doc.cells(sheet, range).unwrap_err();
            assert!(matches!(err, DocsError::Request(_)), "{sheet:?} {range}: {err}");
        }
        assert!(matches!(XlsxDoc::parse(b"PK\x03\x04 not a workbook"), Err(DocsError::Invalid { .. })));
    }

    fn dark_pixels(image: &Rgba, x0: f32, y0: f32, x1: f32, y1: f32) -> usize {
        let mut count = 0;
        for y in y0 as u32..y1 as u32 {
            for x in x0 as u32..x1 as u32 {
                if image.pixel(x, y).is_some_and(|p| p[0] < 128) {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn renders_a_grid_with_headers_and_cell_text_in_place() {
        let doc = XlsxDoc::parse(&workbook()).unwrap();
        assert_eq!(doc.page_count(), 2);
        let size = doc.page_size(0).unwrap();
        assert_eq!(size, PageSize { width: ROW_HEADER_WIDTH + 2.0 * COLUMN_WIDTH, height: 6.0 * ROW_HEIGHT });

        let scale = 2.0;
        let image = doc.render_page(0, scale, &Fonts::none()).unwrap();
        assert_eq!((image.width, image.height), size.pixels(scale));
        let (col_w, row_h, head_w) = (COLUMN_WIDTH * scale, ROW_HEIGHT * scale, ROW_HEADER_WIDTH * scale);
        let header = image.pixel((head_w + 3.0) as u32, 3).unwrap();
        assert_ne!(header, [255; 4]);

        // A2 "apple" is left aligned text, B2 3 is a right aligned number.
        let a2 = (head_w, row_h * 2.0);
        assert!(dark_pixels(&image, a2.0, a2.1, a2.0 + col_w / 2.0, a2.1 + row_h) > 0);
        let b2 = (head_w + col_w, row_h * 2.0);
        assert_eq!(dark_pixels(&image, b2.0 + 1.0, b2.1 + 1.0, b2.0 + col_w / 2.0, b2.1 + row_h - 1.0), 0);
        assert!(dark_pixels(&image, b2.0 + col_w / 2.0, b2.1, b2.0 + col_w, b2.1 + row_h) > 0);

        // Offset: data at D3, A1 is blank, the grid starts at A1.
        let size = doc.page_size(1).unwrap();
        assert_eq!(size, PageSize { width: ROW_HEADER_WIDTH + 5.0 * COLUMN_WIDTH, height: 5.0 * ROW_HEIGHT });
        let image = doc.render_page(1, 1.0, &Fonts::none()).unwrap();
        let cell_rect = |row: f32, col: f32| {
            let x = ROW_HEADER_WIDTH + col * COLUMN_WIDTH;
            let y = (row + 1.0) * ROW_HEIGHT;
            (x + 1.0, y + 1.0, x + COLUMN_WIDTH - 1.0, y + ROW_HEIGHT - 1.0)
        };
        let (x0, y0, x1, y1) = cell_rect(0.0, 0.0);
        assert_eq!(dark_pixels(&image, x0, y0, x1, y1), 0);
        let (x0, y0, x1, y1) = cell_rect(2.0, 3.0);
        assert!(dark_pixels(&image, x0, y0, x1, y1) > 0);

        assert!(matches!(doc.render_page(2, 1.0, &Fonts::none()), Err(DocsError::PageRange { index: 2, count: 2 })));
    }
}
