//! Diff model: numbered lines with intra-line ranges, hunks, folds of unchanged lines, and the
//! row lists the view virtualizes.

use similar::{Algorithm, DiffOp, TextDiff};
use std::ops::Range;
use std::time::Duration;

/// Unchanged lines kept around each change, like `diff -U3`.
pub const CONTEXT: usize = 3;
/// Folds hide at least this many lines; a one-line fold takes the line's own room.
pub const MIN_FOLD: usize = 2;
const LINE_DIFF_TIMEOUT: Duration = Duration::from_secs(2);
/// Paired lines longer than this, or less similar than `MIN_INTRA_RATIO`, get no word ranges.
const MAX_INTRA_LINE: usize = 4_000;
const MIN_INTRA_RATIO: f32 = 0.4;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tag {
    Equal,
    Delete,
    Insert,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line {
    pub tag: Tag,
    /// 1-based; `None` for inserted lines.
    pub old_no: Option<u32>,
    /// 1-based; `None` for deleted lines.
    pub new_no: Option<u32>,
    /// The text (no line ending) in the file's buffer, and its range of emphasis ranges.
    text: Range<usize>,
    emph: Range<usize>,
}

/// A run of unchanged lines hidden behind one row until expanded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fold {
    pub lines: Range<usize>,
    pub expanded: bool,
}

/// A `@@` header of the source: the line starts a new hunk, with this section text.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Break {
    line: usize,
    section: Range<usize>,
}

/// `@@ -old_start,old_len +new_start,new_len @@ section`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    section: Option<usize>,
}

/// One rendered row.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Row {
    Hunk(Hunk),
    /// A collapsed fold and the lines it hides.
    Fold { fold: usize, hidden: usize },
    /// Unified mode: one line.
    Line(usize),
    /// Split mode: an old line beside a new one.
    Pair { old: Option<usize>, new: Option<usize> },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
}

/// The diff of one file.
#[derive(Clone, Debug, Default)]
pub struct FileDiff {
    /// `None` when the file is new.
    pub old_path: Option<String>,
    /// `None` when the file was deleted.
    pub new_path: Option<String>,
    /// The source said the file is binary; there are no lines.
    pub binary: bool,
    text: String,
    lines: Vec<Line>,
    emph: Vec<Range<usize>>,
    folds: Vec<Fold>,
    breaks: Vec<Break>,
}

impl FileDiff {
    /// Diffs two versions of a file; `None` paths mark an added or deleted file.
    pub fn from_texts(old_path: Option<String>, new_path: Option<String>, old: &str, new: &str) -> Self {
        let diff = TextDiff::configure().algorithm(Algorithm::Myers).timeout(LINE_DIFF_TIMEOUT).diff_lines(old, new);
        let mut b = Builder::default();
        let no = |i: usize| Some(count(i + 1));
        for op in diff.ops() {
            if let DiffOp::Equal { old_index, new_index, len } = *op {
                (0..len).for_each(|k| b.push(Tag::Equal, no(old_index + k), no(new_index + k), diff.old_slice(old_index + k).unwrap_or_default()));
                continue;
            }
            op.old_range().for_each(|k| b.push(Tag::Delete, no(k), None, diff.old_slice(k).unwrap_or_default()));
            op.new_range().for_each(|k| b.push(Tag::Insert, None, no(k), diff.new_slice(k).unwrap_or_default()));
        }
        b.finish(old_path, new_path, false)
    }

    /// The new path, or the old one when deleted.
    pub fn path(&self) -> &str { self.new_path.as_deref().or(self.old_path.as_deref()).unwrap_or_default() }

    pub fn status(&self) -> FileStatus {
        match (&self.old_path, &self.new_path) {
            (None, _) => FileStatus::Added,
            (_, None) => FileStatus::Deleted,
            (Some(old), Some(new)) if old != new => FileStatus::Renamed,
            _ => FileStatus::Modified,
        }
    }

    pub fn added(&self) -> usize { self.lines.iter().filter(|l| l.tag == Tag::Insert).count() }
    pub fn removed(&self) -> usize { self.lines.iter().filter(|l| l.tag == Tag::Delete).count() }
    pub fn lines(&self) -> &[Line] { &self.lines }
    pub fn folds(&self) -> &[Fold] { &self.folds }
    pub fn line_text(&self, i: usize) -> &str { self.lines.get(i).map_or("", |l| &self.text[l.text.clone()]) }
    /// Byte ranges of the changed words in line `i`, relative to its text.
    pub fn emphasis(&self, i: usize) -> &[Range<usize>] { self.lines.get(i).map_or(&[], |l| &self.emph[l.emph.clone()]) }
    /// Section text of a hunk header, such as the enclosing function.
    pub fn section(&self, h: &Hunk) -> &str { h.section.and_then(|b| self.breaks.get(b)).map_or("", |b| &self.text[b.section.clone()]) }
    /// Opens a fold; false when it was open or doesn't exist.
    pub fn expand(&mut self, fold: usize) -> bool { self.folds.get_mut(fold).is_some_and(|f| !std::mem::replace(&mut f.expanded, true)) }

    /// Unified rows under the current folds.
    pub fn unified_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        self.walk(|head, lines| rows.extend(std::iter::once(head).chain(lines.map(Row::Line))));
        rows
    }

    /// Side by side rows: a change block's deleted and inserted lines pair up in order and the
    /// longer side gets blank partners.
    pub fn split_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        self.walk(|head, lines| {
            rows.push(head);
            let (mut dels, mut ins) = (Vec::new(), Vec::new());
            let flush = |rows: &mut Vec<Row>, dels: &mut Vec<usize>, ins: &mut Vec<usize>| {
                rows.extend((0..dels.len().max(ins.len())).map(|k| Row::Pair { old: dels.get(k).copied(), new: ins.get(k).copied() }));
                dels.clear();
                ins.clear();
            };
            for i in lines {
                match self.lines[i].tag {
                    Tag::Equal => {
                        flush(&mut rows, &mut dels, &mut ins);
                        rows.push(Row::Pair { old: Some(i), new: Some(i) });
                    }
                    Tag::Delete => dels.push(i),
                    Tag::Insert => ins.push(i),
                }
            }
            flush(&mut rows, &mut dels, &mut ins);
        });
        rows
    }

    /// Feeds the visible structure in order: a hunk header or fold row with the visible lines
    /// after it (none after a fold). A run never crosses a hunk boundary.
    fn walk(&self, mut emit: impl FnMut(Row, Range<usize>)) {
        let mut folds = self.folds.iter().enumerate().filter(|(_, f)| !f.expanded).peekable();
        let mut breaks = self.breaks.iter().enumerate().peekable();
        // The last old and new numbers before `i`, for the empty side of a hunk.
        let (mut i, mut last) = (0, (0, 0));
        while i < self.lines.len() {
            let end = if let Some((index, fold)) = folds.next_if(|(_, f)| f.lines.start == i) {
                emit(Row::Fold { fold: index, hidden: fold.lines.len() }, i..i);
                fold.lines.end
            } else {
                while breaks.next_if(|(_, b)| b.line < i).is_some() {}
                let section = breaks.next_if(|(_, b)| b.line == i).map(|(index, _)| index);
                let end = folds.peek().map_or(self.lines.len(), |(_, f)| f.lines.start).min(breaks.peek().map_or(self.lines.len(), |(_, b)| b.line));
                let lines = &self.lines[i..end];
                let (old, new) = (lines.iter().filter_map(|l| l.old_no), lines.iter().filter_map(|l| l.new_no));
                let hunk = Hunk { old_start: old.clone().next().unwrap_or(last.0), old_len: count(old.count()), new_start: new.clone().next().unwrap_or(last.1), new_len: count(new.count()), section };
                emit(Row::Hunk(hunk), i..end);
                end
            };
            (last, i) = (self.last_numbers(end, last), end);
        }
    }

    fn last_numbers(&self, end: usize, fallback: (u32, u32)) -> (u32, u32) {
        let before = self.lines[..end].iter().rev();
        (before.clone().find_map(|l| l.old_no).unwrap_or(fallback.0), before.clone().find_map(|l| l.new_no).unwrap_or(fallback.1))
    }
}

/// Counts above `u32::MAX` saturate; no text the panel shows is that long.
fn count(n: usize) -> u32 { u32::try_from(n).unwrap_or(u32::MAX) }

/// Collects lines into a [`FileDiff`], pairing each change block's deleted and inserted lines
/// for intra-line ranges.
#[derive(Default)]
pub(crate) struct Builder {
    text: String,
    lines: Vec<Line>,
    emph: Vec<Range<usize>>,
    breaks: Vec<Break>,
    dels: Vec<usize>,
    ins: Vec<usize>,
}

impl Builder {
    /// Adds a line; `text` may still carry its line ending.
    pub(crate) fn push(&mut self, tag: Tag, old_no: Option<u32>, new_no: Option<u32>, text: &str) {
        if tag == Tag::Equal || (tag == Tag::Delete && !self.ins.is_empty()) { self.flush_block() }
        let start = self.text.len();
        self.text.push_str(text.trim_end_matches('\n').trim_end_matches('\r'));
        let (i, e) = (self.lines.len(), self.emph.len());
        self.lines.push(Line { tag, old_no, new_no, text: start..self.text.len(), emph: e..e });
        match tag {
            Tag::Delete => self.dels.push(i),
            Tag::Insert => self.ins.push(i),
            Tag::Equal => {}
        }
    }

    /// Starts a new hunk at the next line, as a `@@` header does.
    pub(crate) fn hunk_break(&mut self, section: &str) {
        self.flush_block();
        let start = self.text.len();
        self.text.push_str(section);
        self.breaks.push(Break { line: self.lines.len(), section: start..self.text.len() });
    }

    /// A parsed diff starts and ends at hunk edges whose context belongs to it; a text diff
    /// starts and ends at the file edges.
    pub(crate) fn finish(mut self, old_path: Option<String>, new_path: Option<String>, binary: bool) -> FileDiff {
        self.flush_block();
        let folds = compute_folds(&self.lines, &self.breaks, !self.breaks.is_empty());
        FileDiff { old_path, new_path, binary, text: self.text, lines: self.lines, emph: self.emph, folds, breaks: self.breaks }
    }

    fn flush_block(&mut self) {
        for (&old, &new) in self.dels.iter().zip(&self.ins) {
            let Some(Intra { old: o, new: n }) = intra_line(&self.text[self.lines[old].text.clone()], &self.text[self.lines[new].text.clone()]) else { continue };
            for (line, ranges) in [(old, o), (new, n)] {
                let start = self.emph.len();
                self.emph.extend(ranges);
                self.lines[line].emph = start..self.emph.len();
            }
        }
        self.dels.clear();
        self.ins.clear();
    }
}

/// Byte ranges that differ between two paired lines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Intra {
    pub old: Vec<Range<usize>>,
    pub new: Vec<Range<usize>>,
}

/// Changed words of two paired lines; `None` when too long, too different, or one side
/// changed entirely.
pub fn intra_line(old: &str, new: &str) -> Option<Intra> {
    if old.len() > MAX_INTRA_LINE || new.len() > MAX_INTRA_LINE || old.is_empty() || new.is_empty() { return None }
    let diff = TextDiff::configure().algorithm(Algorithm::Myers).diff_words(old, new);
    if diff.ratio() < MIN_INTRA_RATIO { return None }
    fn offsets<'a>(tokens: impl Iterator<Item = &'a str>) -> Vec<usize> { std::iter::once(0).chain(tokens.scan(0, |at, t| { *at += t.len(); Some(*at) })).collect() }
    let (oo, no) = (offsets(diff.iter_old_slices()), offsets(diff.iter_new_slices()));
    let (mut old_r, mut new_r): (Vec<Range<usize>>, Vec<Range<usize>>) = (Vec::new(), Vec::new());
    for op in diff.ops().iter().filter(|op| !matches!(op, DiffOp::Equal { .. })) {
        let (o, n) = (op.old_range(), op.new_range());
        add_range(&mut old_r, oo[o.start]..oo[o.end]);
        add_range(&mut new_r, no[n.start]..no[n.end]);
    }
    let whole = |r: &[Range<usize>], len| r.len() == 1 && r[0] == (0..len);
    (!(old_r.is_empty() && new_r.is_empty()) && !whole(&old_r, old.len()) && !whole(&new_r, new.len())).then_some(Intra { old: old_r, new: new_r })
}

/// Appends `range`, merging it into the previous one when they touch.
fn add_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    match ranges.last_mut() {
        _ if range.is_empty() => {}
        Some(last) if last.end == range.start => last.end = range.end,
        _ => ranges.push(range),
    }
}

/// Folds every run of unchanged lines farther than [`CONTEXT`] from a change. Hunk breaks
/// count as changes on both sides; file edges only when `edges_are_context`.
fn compute_folds(lines: &[Line], breaks: &[Break], edges_are_context: bool) -> Vec<Fold> {
    let (mut folds, mut breaks, mut i) = (Vec::new(), breaks.iter().map(|b| b.line).peekable(), 0);
    while i < lines.len() {
        if lines[i].tag != Tag::Equal {
            i += 1;
            continue;
        }
        let start = i;
        while breaks.next_if(|&b| b <= start).is_some() {}
        // A run of unchanged lines also ends at a hunk break.
        let limit = breaks.peek().copied().unwrap_or(lines.len());
        while i < limit && lines[i].tag == Tag::Equal { i += 1 }
        let before = if start > 0 || edges_are_context { CONTEXT } else { 0 };
        let after = if i < lines.len() || edges_are_context { CONTEXT } else { 0 };
        let hidden = (start + before)..i.saturating_sub(after);
        if hidden.len() >= MIN_FOLD { folds.push(Fold { lines: hidden, expanded: false }) }
    }
    folds
}
