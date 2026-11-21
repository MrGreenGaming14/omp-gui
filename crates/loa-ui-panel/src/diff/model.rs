//! Diff model: numbered lines with intra-line ranges, hunks, folds of unchanged lines, and the
//! row lists the view virtualizes.

use similar::{Algorithm, DiffOp, TextDiff};
use std::ops::Range;

/// Unchanged lines kept around each change, like `diff -U3`.
const CONTEXT: usize = 3;
/// Folds hide at least this many lines; a one-line fold takes the line's own room.
const MIN_FOLD: usize = 2;
/// Paired lines longer than this, or less similar than `MIN_INTRA_RATIO`, get no word ranges.
const MAX_INTRA_LINE: usize = 4_000;
const MIN_INTRA_RATIO: f32 = 0.4;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tag { Equal, Delete, Insert }

/// A line with 1-based numbers (`None` on the side it is missing from), its text in the file's
/// buffer and its range of emphasis ranges.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line { pub tag: Tag, pub old_no: Option<u32>, pub new_no: Option<u32>, text: Range<usize>, emph: Range<usize> }

/// A run of unchanged lines hidden behind one row until expanded.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Fold { lines: Range<usize>, expanded: bool }

/// A `@@` header of the source: `line` starts a new hunk with this section text.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Break { line: usize, section: Range<usize> }

/// `@@ -old_start,old_len +new_start,new_len @@ section`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Hunk { pub old_start: u32, pub old_len: u32, pub new_start: u32, pub new_len: u32, section: Option<usize> }

/// One rendered row: a hunk header, a collapsed fold, a unified line or a split pair.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Row { Hunk(Hunk), Fold { fold: usize, hidden: usize }, Line(usize), Pair { old: Option<usize>, new: Option<usize> } }

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FileStatus { Added, Deleted, Modified, Renamed }

/// The diff of one file; a `None` path marks an added or deleted file, and a binary one has no lines.
#[derive(Clone, Debug, Default)]
pub struct FileDiff {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub binary: bool,
    text: String,
    lines: Vec<Line>,
    emph: Vec<Range<usize>>,
    folds: Vec<Fold>,
    breaks: Vec<Break>,
}

impl FileDiff {
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
    pub fn line_text(&self, i: usize) -> &str { self.lines.get(i).map_or("", |l| &self.text[l.text.clone()]) }
    /// Byte ranges of the changed words in line `i`, relative to its text.
    pub fn emphasis(&self, i: usize) -> &[Range<usize>] { self.lines.get(i).map_or(&[], |l| &self.emph[l.emph.clone()]) }
    /// Section text of a hunk header, such as the enclosing function.
    pub fn section(&self, h: &Hunk) -> &str { h.section.and_then(|b| self.breaks.get(b)).map_or("", |b| &self.text[b.section.clone()]) }
    /// Opens a fold; false when it was open or doesn't exist.
    pub fn expand(&mut self, fold: usize) -> bool { self.folds.get_mut(fold).is_some_and(|f| !std::mem::replace(&mut f.expanded, true)) }

    pub fn unified_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        self.walk(|head, lines| rows.extend(std::iter::once(head).chain(lines.map(Row::Line))));
        rows
    }

    /// Side by side rows: a change run's deleted and inserted lines pair up in order and the
    /// longer side gets blank partners.
    pub fn split_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        self.walk(|head, lines| {
            rows.push(head);
            let lines: Vec<usize> = lines.collect();
            for run in lines.chunk_by(|&a, &b| (self.lines[a].tag == Tag::Equal) == (self.lines[b].tag == Tag::Equal)) {
                let (old, new): (Vec<usize>, Vec<usize>) = run.iter().partition(|&&i| self.lines[i].tag != Tag::Insert);
                let new = if self.lines[run[0]].tag == Tag::Equal { &old } else { &new };
                rows.extend((0..old.len().max(new.len())).map(|k| Row::Pair { old: old.get(k).copied(), new: new.get(k).copied() }));
            }
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
            let before = self.lines[..end].iter().rev();
            last = (before.clone().find_map(|l| l.old_no).unwrap_or(last.0), before.clone().find_map(|l| l.new_no).unwrap_or(last.1));
            i = end;
        }
    }
}

/// Counts above `u32::MAX` saturate; no text the panel shows is that long.
fn count(n: usize) -> u32 { u32::try_from(n).unwrap_or(u32::MAX) }

/// Collects lines into a [`FileDiff`], pairing each change block's deleted and inserted lines
/// for intra-line ranges.
#[derive(Default)]
pub(crate) struct Builder {
    file: FileDiff,
    dels: Vec<usize>,
    ins: Vec<usize>,
}

impl Builder {
    pub(crate) fn push(&mut self, tag: Tag, old_no: Option<u32>, new_no: Option<u32>, text: &str) {
        if tag == Tag::Equal || (tag == Tag::Delete && !self.ins.is_empty()) { self.flush_block() }
        let f = &mut self.file;
        let (start, i, e) = (f.text.len(), f.lines.len(), f.emph.len());
        f.text.push_str(text);
        f.lines.push(Line { tag, old_no, new_no, text: start..f.text.len(), emph: e..e });
        if tag != Tag::Equal { if tag == Tag::Delete { &mut self.dels } else { &mut self.ins }.push(i) }
    }

    /// Starts a new hunk at the next line, as a `@@` header does.
    pub(crate) fn hunk_break(&mut self, section: &str) {
        self.flush_block();
        let f = &mut self.file;
        let start = f.text.len();
        f.text.push_str(section);
        f.breaks.push(Break { line: f.lines.len(), section: start..f.text.len() });
    }

    pub(crate) fn finish(mut self, old_path: Option<String>, new_path: Option<String>, binary: bool) -> FileDiff {
        self.flush_block();
        let folds = compute_folds(&self.file.lines, &self.file.breaks);
        FileDiff { old_path, new_path, binary, folds, ..self.file }
    }

    fn flush_block(&mut self) {
        let f = &mut self.file;
        for (&old, &new) in self.dels.iter().zip(&self.ins) {
            let Some(ranges) = intra_line(&f.text[f.lines[old].text.clone()], &f.text[f.lines[new].text.clone()]) else { continue };
            for (line, ranges) in [old, new].into_iter().zip(ranges) {
                let start = f.emph.len();
                f.emph.extend(ranges);
                f.lines[line].emph = start..f.emph.len();
            }
        }
        self.dels.clear();
        self.ins.clear();
    }
}

/// Changed byte ranges of two paired lines, old then new; `None` when too long, too
/// different, or one side changed entirely.
fn intra_line(old: &str, new: &str) -> Option<[Vec<Range<usize>>; 2]> {
    if old.len() > MAX_INTRA_LINE || new.len() > MAX_INTRA_LINE || old.is_empty() || new.is_empty() { return None }
    let diff = TextDiff::configure().algorithm(Algorithm::Myers).diff_words(old, new);
    if diff.ratio() < MIN_INTRA_RATIO { return None }
    fn offsets<'a>(tokens: impl Iterator<Item = &'a str>) -> Vec<usize> { std::iter::once(0).chain(tokens.scan(0, |at, t| { *at += t.len(); Some(*at) })).collect() }
    let offsets = [offsets(diff.iter_old_slices()), offsets(diff.iter_new_slices())];
    let mut ranges: [Vec<Range<usize>>; 2] = Default::default();
    for op in diff.ops().iter().filter(|op| !matches!(op, DiffOp::Equal { .. })) {
        for ((ranges, at), r) in ranges.iter_mut().zip(&offsets).zip([op.old_range(), op.new_range()]) {
            match ranges.last_mut() {
                _ if r.is_empty() => {}
                Some(last) if last.end == at[r.start] => last.end = at[r.end],
                _ => ranges.push(at[r.start]..at[r.end]),
            }
        }
    }
    let whole = |r: &[Range<usize>], len| r.len() == 1 && r[0] == (0..len);
    (!(ranges[0].is_empty() && ranges[1].is_empty()) && !whole(&ranges[0], old.len()) && !whole(&ranges[1], new.len())).then_some(ranges)
}

/// Folds every run of unchanged lines farther than [`CONTEXT`] from a change or a hunk edge.
fn compute_folds(lines: &[Line], breaks: &[Break]) -> Vec<Fold> {
    let (mut folds, mut breaks, mut i) = (Vec::new(), breaks.iter().map(|b| b.line).peekable(), 0);
    while i < lines.len() {
        let start = i;
        while breaks.next_if(|&b| b <= start).is_some() {}
        // A run of unchanged lines also ends at a hunk break.
        let limit = breaks.peek().copied().unwrap_or(lines.len());
        while i < limit && lines[i].tag == Tag::Equal { i += 1 }
        let hidden = (start + CONTEXT)..i.saturating_sub(CONTEXT);
        if hidden.len() >= MIN_FOLD { folds.push(Fold { lines: hidden, expanded: false }) }
        i = i.max(start + 1);
    }
    folds
}
