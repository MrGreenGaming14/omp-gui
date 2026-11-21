//! Parser for unified diffs, plain (`diff -u`) or git style, with one or many files.

use super::model::{Builder, FileDiff, Tag};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ParseError {
    #[error("line {line}: malformed hunk header")]
    HunkHeader { line: usize },
    #[error("line {line}: hunk or file header outside a file")]
    Orphan { line: usize },
    #[error("line {line}: hunk has more lines than its header says")]
    Overflow { line: usize },
    #[error("line {line}: hunk ends with {old} old and {new} new lines missing")]
    Truncated { line: usize, old: u32, new: u32 },
}

pub type Result<T, E = ParseError> = std::result::Result<T, E>;

/// A file being collected; `git` when `diff --git` started it, so `---`/`+++` belong to it.
#[derive(Default)]
struct Pending { old_path: Option<String>, new_path: Option<String>, git: bool, has_hunks: bool, binary: bool, builder: Builder }

impl Pending {
    fn finish(self) -> FileDiff { self.builder.finish(self.old_path, self.new_path, self.binary) }
}

/// Lines still expected in the current hunk (old, new), and the next line numbers.
#[derive(Copy, Clone)]
struct Hunk { left: [u32; 2], next: [u32; 2] }

/// One [`FileDiff`] per file; text before the first file header (a commit message) is skipped.
pub fn parse_unified(input: &str) -> Result<Vec<FileDiff>> {
    let (mut files, mut current, mut hunk, mut last) = (Vec::new(), None::<Pending>, None::<Hunk>, 0);
    for (index, raw) in input.split_inclusive('\n').enumerate() {
        let (number, line) = (index + 1, raw.trim_end_matches('\n').trim_end_matches('\r'));
        last = number;
        if let Some(h) = hunk.as_mut().filter(|h| h.left != [0, 0]) {
            hunk_line(&mut current.as_mut().ok_or(ParseError::Orphan { line: number })?.builder, h, line, number)?;
            continue;
        }
        hunk = None;
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.extend(current.take().map(Pending::finish));
            let rest = rest.trim_end();
            let (old, new) = rest.strip_prefix("a/").and_then(|r| r.rsplit_once(" b/")).unwrap_or_else(|| rest.split_once(' ').unwrap_or((rest, rest)));
            current = Some(Pending { old_path: Some(old.to_owned()), new_path: Some(new.to_owned()), git: true, ..Pending::default() });
        } else if let Some(rest) = line.strip_prefix("--- ") {
            match current.as_mut() {
                Some(f) if f.git && !f.has_hunks => f.old_path = header_path(rest, "a/"),
                _ => files.extend(current.replace(Pending { old_path: header_path(rest, "a/"), ..Pending::default() }).map(Pending::finish)),
            }
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            current.as_mut().ok_or(ParseError::Orphan { line: number })?.new_path = header_path(rest, "b/");
        } else if let Some(rest) = line.strip_prefix("@@ ") {
            let f = current.as_mut().ok_or(ParseError::Orphan { line: number })?;
            let (h, section) = hunk_header(rest).ok_or(ParseError::HunkHeader { line: number })?;
            f.builder.hunk_break(section);
            (f.has_hunks, hunk) = (true, Some(h));
        } else if let Some(f) = current.as_mut() {
            // Git's extended header lines between `diff --git` and the hunks.
            if let Some(p) = line.strip_prefix("rename from ") {
                f.old_path = Some(p.to_owned());
            } else if let Some(p) = line.strip_prefix("rename to ") {
                f.new_path = Some(p.to_owned());
            } else if line.starts_with("new file mode") {
                f.old_path = None;
            } else if line.starts_with("deleted file mode") {
                f.new_path = None;
            } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
                f.binary = true;
            }
        }
    }
    if let Some(h) = hunk.filter(|h| h.left != [0, 0]) { return Err(ParseError::Truncated { line: last, old: h.left[0], new: h.left[1] }) }
    files.extend(current.map(Pending::finish));
    Ok(files)
}

fn hunk_line(b: &mut Builder, h: &mut Hunk, line: &str, number: usize) -> Result<()> {
    let (marker, text) = line.char_indices().nth(1).map_or((line, ""), |(at, _)| (&line[..at], &line[at..]));
    let (tag, sides) = match marker {
        // Some tools strip the space of empty context lines.
        " " | "" => (Tag::Equal, [true, true]),
        "-" => (Tag::Delete, [true, false]),
        "+" => (Tag::Insert, [false, true]),
        // "\ No newline at end of file"
        "\\" => return Ok(()),
        _ => return Err(ParseError::Truncated { line: number, old: h.left[0], new: h.left[1] }),
    };
    for (left, _) in h.left.iter_mut().zip(sides).filter(|s| s.1) {
        *left = left.checked_sub(1).ok_or(ParseError::Overflow { line: number })?;
    }
    b.push(tag, sides[0].then_some(h.next[0]), sides[1].then_some(h.next[1]), text);
    h.next = [0, 1].map(|side| h.next[side] + u32::from(sides[side]));
    Ok(())
}

/// Parses `-a[,b] +c[,d] @@ section`; a zero length side names the line before the hunk.
fn hunk_header(rest: &str) -> Option<(Hunk, &str)> {
    let (ranges, section) = rest.split_once("@@")?;
    let range = |t: &str| match t.split_once(',') {
        Some((s, l)) => Some((s.parse().ok()?, l.parse().ok()?)),
        None => Some((t.parse().ok()?, 1)),
    };
    let mut parts = ranges.split_whitespace();
    let ((os, ol), (ns, nl)) = (range(parts.next()?.strip_prefix('-')?)?, range(parts.next()?.strip_prefix('+')?)?);
    let first = |s: u32, l: u32| if l == 0 { s + 1 } else { s };
    parts.next().is_none().then(|| (Hunk { left: [ol, nl], next: [first(os, ol), first(ns, nl)] }, section.trim_start()))
}

/// The path of a `---`/`+++` line without timestamp or `a/`/`b/`; `/dev/null` is no file.
fn header_path(rest: &str, prefix: &str) -> Option<String> {
    let path = rest.split('\t').next().unwrap_or(rest).trim_end();
    (path != "/dev/null").then(|| path.strip_prefix(prefix).unwrap_or(path).to_owned())
}
