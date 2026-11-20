//! Diff model: lines with numbers and intra-line ranges, hunks, folds of
//! unchanged lines, and the row lists the view virtualizes.

use similar::{Algorithm, DiffOp, TextDiff};
use std::ops::Range;
use std::time::Duration;

/// Unchanged lines kept around each change, like `diff -U3`.
pub const CONTEXT: usize = 3;

/// Folds hide at least this many lines. A fold of one line would take the
/// same room as the line itself.
pub const MIN_FOLD: usize = 2;

/// Line diffs give up refining after this long and report a coarser diff.
const LINE_DIFF_TIMEOUT: Duration = Duration::from_secs(2);

/// Paired lines longer than this get no intra-line ranges.
const MAX_INTRA_LINE: usize = 4_000;

/// Paired lines less similar than this are shown as wholly changed.
const MIN_INTRA_RATIO: f32 = 0.4;

/// What happened to one line.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tag {
    Equal,
    Delete,
    Insert,
}

/// One line of a file diff.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line {
    pub tag: Tag,
    /// 1-based line number in the old file. `None` for inserted lines.
    pub old_no: Option<u32>,
    /// 1-based line number in the new file. `None` for deleted lines.
    pub new_no: Option<u32>,
    /// Byte range of the line text (no line ending) in [`FileDiff::text`].
    text: Range<usize>,
    /// Range in [`FileDiff`]'s emphasis list.
    emph: Range<usize>,
}

/// A run of unchanged lines hidden behind one row until expanded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fold {
    /// Line indices covered.
    pub lines: Range<usize>,
    pub expanded: bool,
}

/// Where the source diff jumps: the line starts a new hunk even when no fold
/// sits before it. Parsed diffs have one per `@@` header.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Break {
    line: usize,
    /// Section text after the closing `@@`, as a range in the text buffer.
    section: Range<usize>,
}

/// A hunk header row: `@@ -old_start,old_len +new_start,new_len @@ section`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    /// Index of the break whose section text this hunk shows.
    section: Option<usize>,
}

/// One row of the rendered diff.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Row {
    Hunk(Hunk),
    /// A collapsed fold: index into [`FileDiff::folds`] and the lines hidden.
    Fold { fold: usize, hidden: usize },
    /// Unified mode: one line.
    Line(usize),
    /// Split mode: an old line on the left, a new line on the right.
    Pair { old: Option<usize>, new: Option<usize> },
}

/// How a file changed.
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
    added: usize,
    removed: usize,
    /// Largest line number on either side; sizes the number column.
    max_line_no: u32,
}

impl FileDiff {
    /// Diffs two versions of a file. `None` paths mark an added or deleted
    /// file.
    pub fn from_texts(old_path: Option<String>, new_path: Option<String>, old: &str, new: &str) -> Self {
        let diff = TextDiff::configure()
            .algorithm(Algorithm::Myers)
            .timeout(LINE_DIFF_TIMEOUT)
            .diff_lines(old, new);
        let mut builder = Builder::default();
        let old_line = |i: usize| diff.old_slice(i).unwrap_or_default();
        let new_line = |i: usize| diff.new_slice(i).unwrap_or_default();
        for op in diff.ops() {
            match *op {
                DiffOp::Equal { old_index, new_index, len } => {
                    for k in 0..len {
                        builder.push(Tag::Equal, line_no(old_index + k), line_no(new_index + k), old_line(old_index + k));
                    }
                }
                DiffOp::Delete { old_index, old_len, .. } => {
                    for k in old_index..old_index + old_len {
                        builder.push(Tag::Delete, line_no(k), None, old_line(k));
                    }
                }
                DiffOp::Insert { new_index, new_len, .. } => {
                    for k in new_index..new_index + new_len {
                        builder.push(Tag::Insert, None, line_no(k), new_line(k));
                    }
                }
                DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                    for k in old_index..old_index + old_len {
                        builder.push(Tag::Delete, line_no(k), None, old_line(k));
                    }
                    for k in new_index..new_index + new_len {
                        builder.push(Tag::Insert, None, line_no(k), new_line(k));
                    }
                }
            }
        }
        builder.finish(old_path, new_path, false)
    }

    /// Path shown for the file: the new path, or the old one when deleted.
    pub fn path(&self) -> &str {
        self.new_path.as_deref().or(self.old_path.as_deref()).unwrap_or_default()
    }

    pub fn status(&self) -> FileStatus {
        match (&self.old_path, &self.new_path) {
            (None, _) => FileStatus::Added,
            (_, None) => FileStatus::Deleted,
            (Some(old), Some(new)) if old != new => FileStatus::Renamed,
            _ => FileStatus::Modified,
        }
    }

    /// Lines added.
    pub fn added(&self) -> usize {
        self.added
    }

    /// Lines removed.
    pub fn removed(&self) -> usize {
        self.removed
    }

    /// Largest line number on either side. Zero when no line is numbered.
    pub fn max_line_no(&self) -> u32 {
        self.max_line_no
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn folds(&self) -> &[Fold] {
        &self.folds
    }

    /// Text of line `index` without its line ending.
    pub fn line_text(&self, index: usize) -> &str {
        self.lines.get(index).map_or("", |line| &self.text[line.text.clone()])
    }

    /// Byte ranges of the changed parts inside line `index`, relative to its
    /// text. Empty when the whole line changed or it is unchanged.
    pub fn emphasis(&self, index: usize) -> &[Range<usize>] {
        self.lines.get(index).map_or(&[], |line| &self.emph[line.emph.clone()])
    }

    /// Section text of a hunk header, such as the enclosing function.
    pub fn section(&self, hunk: &Hunk) -> &str {
        hunk.section.and_then(|b| self.breaks.get(b)).map_or("", |b| &self.text[b.section.clone()])
    }

    /// Expands a fold. Returns false when it was already open or doesn't
    /// exist.
    pub fn expand(&mut self, fold: usize) -> bool {
        match self.folds.get_mut(fold) {
            Some(f) if !f.expanded => {
                f.expanded = true;
                true
            }
            _ => false,
        }
    }

    /// Rows for unified mode under the current fold state.
    pub fn unified_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        self.walk(|segment| match segment {
            Segment::Head(row) => rows.push(row),
            Segment::Lines(range) => rows.extend(range.map(Row::Line)),
        });
        rows
    }

    /// Rows for side by side mode under the current fold state. Deleted and
    /// inserted lines of one change block pair up in order; the longer side
    /// gets blank partners.
    pub fn split_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut deletes = Vec::new();
        let mut inserts = Vec::new();
        self.walk(|segment| match segment {
            Segment::Head(row) => rows.push(row),
            Segment::Lines(range) => {
                for i in range {
                    match self.lines[i].tag {
                        Tag::Equal => {
                            flush_pairs(&mut rows, &mut deletes, &mut inserts);
                            rows.push(Row::Pair { old: Some(i), new: Some(i) });
                        }
                        Tag::Delete => deletes.push(i),
                        Tag::Insert => inserts.push(i),
                    }
                }
                flush_pairs(&mut rows, &mut deletes, &mut inserts);
            }
        });
        rows
    }

    /// Feeds the visible structure in order: a hunk header or fold row, then
    /// runs of visible lines. A run never crosses a hunk boundary.
    fn walk(&self, mut emit: impl FnMut(Segment)) {
        let mut folds = self.folds.iter().enumerate().filter(|(_, f)| !f.expanded).peekable();
        let mut breaks = self.breaks.iter().enumerate().peekable();
        let mut i = 0;
        // Last old and new line numbers before `i`, for empty sides of hunks.
        let mut last = (0, 0);
        while i < self.lines.len() {
            if let Some((index, fold)) = folds.next_if(|(_, f)| f.lines.start == i) {
                emit(Segment::Head(Row::Fold { fold: index, hidden: fold.lines.len() }));
                last = self.last_numbers(fold.lines.end, last);
                i = fold.lines.end;
                continue;
            }
            while breaks.next_if(|(_, b)| b.line < i).is_some() {}
            let section = breaks.next_if(|(_, b)| b.line == i).map(|(index, _)| index);
            let next_fold = folds.peek().map_or(self.lines.len(), |(_, f)| f.lines.start);
            let next_break = breaks.peek().map_or(self.lines.len(), |(_, b)| b.line);
            let end = next_fold.min(next_break);
            let hunk = self.hunk(i..end, last, section);
            emit(Segment::Head(Row::Hunk(hunk)));
            emit(Segment::Lines(i..end));
            last = self.last_numbers(end, last);
            i = end;
        }
    }

    /// Header numbers for the lines in `range`, like `diff -u`: an empty side
    /// starts at the line before it.
    fn hunk(&self, range: Range<usize>, last: (u32, u32), section: Option<usize>) -> Hunk {
        let lines = &self.lines[range];
        let old_start = lines.iter().find_map(|l| l.old_no).unwrap_or(last.0);
        let new_start = lines.iter().find_map(|l| l.new_no).unwrap_or(last.1);
        let old_len = count(lines.iter().filter(|l| l.old_no.is_some()).count());
        let new_len = count(lines.iter().filter(|l| l.new_no.is_some()).count());
        Hunk { old_start, old_len, new_start, new_len, section }
    }

    /// Last old and new line numbers among the lines before `end`.
    fn last_numbers(&self, end: usize, fallback: (u32, u32)) -> (u32, u32) {
        let before = &self.lines[..end];
        let old = before.iter().rev().find_map(|l| l.old_no).unwrap_or(fallback.0);
        let new = before.iter().rev().find_map(|l| l.new_no).unwrap_or(fallback.1);
        (old, new)
    }
}

enum Segment {
    Head(Row),
    Lines(Range<usize>),
}

fn flush_pairs(rows: &mut Vec<Row>, deletes: &mut Vec<usize>, inserts: &mut Vec<usize>) {
    let n = deletes.len().max(inserts.len());
    rows.extend((0..n).map(|k| Row::Pair { old: deletes.get(k).copied(), new: inserts.get(k).copied() }));
    deletes.clear();
    inserts.clear();
}

fn line_no(index: usize) -> Option<u32> {
    Some(count(index + 1))
}

/// Line counts above `u32::MAX` saturate; no text the panel shows is that long.
fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Strips one trailing line ending.
fn trim_eol(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// Collects lines into a [`FileDiff`], pairing each change block's deleted
/// and inserted lines for intra-line ranges.
#[derive(Default)]
pub(crate) struct Builder {
    text: String,
    lines: Vec<Line>,
    emph: Vec<Range<usize>>,
    breaks: Vec<Break>,
    deletes: Vec<usize>,
    inserts: Vec<usize>,
}

impl Builder {
    /// Adds one line. `text` may still carry its line ending.
    pub(crate) fn push(&mut self, tag: Tag, old_no: Option<u32>, new_no: Option<u32>, text: &str) {
        match tag {
            Tag::Equal => self.flush_block(),
            Tag::Delete if !self.inserts.is_empty() => self.flush_block(),
            _ => {}
        }
        let start = self.text.len();
        self.text.push_str(trim_eol(text));
        let index = self.lines.len();
        let emph = self.emph.len()..self.emph.len();
        self.lines.push(Line { tag, old_no, new_no, text: start..self.text.len(), emph });
        match tag {
            Tag::Delete => self.deletes.push(index),
            Tag::Insert => self.inserts.push(index),
            Tag::Equal => {}
        }
    }

    /// Starts a new hunk at the next line, as a `@@` header in the source does.
    pub(crate) fn hunk_break(&mut self, section: &str) {
        self.flush_block();
        let start = self.text.len();
        self.text.push_str(section);
        self.breaks.push(Break { line: self.lines.len(), section: start..self.text.len() });
    }

    pub(crate) fn finish(mut self, old_path: Option<String>, new_path: Option<String>, binary: bool) -> FileDiff {
        self.flush_block();
        let added = self.lines.iter().filter(|l| l.tag == Tag::Insert).count();
        let removed = self.lines.iter().filter(|l| l.tag == Tag::Delete).count();
        // A parsed diff starts and ends at hunk edges whose context belongs
        // to the diff; a full text diff starts and ends at the file edges.
        let edges_are_context = !self.breaks.is_empty();
        let folds = compute_folds(&self.lines, &self.breaks, CONTEXT, edges_are_context);
        let max_line_no = self.lines.iter().flat_map(|l| l.old_no.max(l.new_no)).max().unwrap_or(0);
        FileDiff {
            old_path,
            new_path,
            binary,
            text: self.text,
            lines: self.lines,
            emph: self.emph,
            folds,
            breaks: self.breaks,
            added,
            removed,
            max_line_no,
        }
    }

    /// Computes intra-line ranges for the pending change block.
    fn flush_block(&mut self) {
        for k in 0..self.deletes.len().min(self.inserts.len()) {
            let (old, new) = (self.deletes[k], self.inserts[k]);
            let old_text = &self.text[self.lines[old].text.clone()];
            let new_text = &self.text[self.lines[new].text.clone()];
            let Some(Intra { old: old_ranges, new: new_ranges }) = intra_line(old_text, new_text) else { continue };
            let start = self.emph.len();
            self.emph.extend(old_ranges);
            self.lines[old].emph = start..self.emph.len();
            let start = self.emph.len();
            self.emph.extend(new_ranges);
            self.lines[new].emph = start..self.emph.len();
        }
        self.deletes.clear();
        self.inserts.clear();
    }
}

/// Byte ranges that differ between two paired lines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Intra {
    pub old: Vec<Range<usize>>,
    pub new: Vec<Range<usize>>,
}

/// Changed parts of two paired lines, by word. `None` when the lines are too
/// long, too different, or one side changed entirely.
pub fn intra_line(old: &str, new: &str) -> Option<Intra> {
    if old.len() > MAX_INTRA_LINE || new.len() > MAX_INTRA_LINE || old.is_empty() || new.is_empty() {
        return None;
    }
    let diff = TextDiff::configure().algorithm(Algorithm::Myers).diff_words(old, new);
    if diff.ratio() < MIN_INTRA_RATIO {
        return None;
    }
    let old_offsets = prefix_offsets(diff.iter_old_slices());
    let new_offsets = prefix_offsets(diff.iter_new_slices());
    let mut old_ranges: Vec<Range<usize>> = Vec::new();
    let mut new_ranges: Vec<Range<usize>> = Vec::new();
    for op in diff.ops() {
        let (old_range, new_range) = (op.old_range(), op.new_range());
        if matches!(op, DiffOp::Equal { .. }) {
            continue;
        }
        add_range(&mut old_ranges, old_offsets[old_range.start]..old_offsets[old_range.end]);
        add_range(&mut new_ranges, new_offsets[new_range.start]..new_offsets[new_range.end]);
    }
    let whole = |ranges: &[Range<usize>], len: usize| ranges.len() == 1 && ranges[0] == (0..len);
    if (old_ranges.is_empty() && new_ranges.is_empty()) || whole(&old_ranges, old.len()) || whole(&new_ranges, new.len())
    {
        return None;
    }
    Some(Intra { old: old_ranges, new: new_ranges })
}

/// Byte offset of each token's start, plus the total length at the end.
fn prefix_offsets<'a>(tokens: impl Iterator<Item = &'a str>) -> Vec<usize> {
    let mut at = 0;
    let mut offsets = vec![0];
    for token in tokens {
        at += token.len();
        offsets.push(at);
    }
    offsets
}

/// Appends `range`, merging it into the previous one when they touch.
fn add_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    if range.is_empty() {
        return;
    }
    match ranges.last_mut() {
        Some(last) if last.end == range.start => last.end = range.end,
        _ => ranges.push(range),
    }
}

/// Folds every run of unchanged lines farther than `context` from a change.
/// Hunk breaks count as changes on both sides; file edges count as changes
/// only when `edges_are_context`.
fn compute_folds(lines: &[Line], breaks: &[Break], context: usize, edges_are_context: bool) -> Vec<Fold> {
    let mut folds = Vec::new();
    let mut breaks = breaks.iter().map(|b| b.line).peekable();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].tag != Tag::Equal {
            i += 1;
            continue;
        }
        let start = i;
        while breaks.next_if(|&b| b <= start).is_some() {}
        // A run of unchanged lines also ends at a hunk break.
        let limit = breaks.peek().copied().unwrap_or(lines.len());
        while i < limit && lines[i].tag == Tag::Equal {
            i += 1;
        }
        let keep_before = if start > 0 || edges_are_context { context } else { 0 };
        let keep_after = if i < lines.len() || edges_are_context { context } else { 0 };
        let hidden = (start + keep_before)..i.saturating_sub(keep_after);
        if hidden.len() >= MIN_FOLD {
            folds.push(Fold { lines: hidden, expanded: false });
        }
    }
    folds
}
