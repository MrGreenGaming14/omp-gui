//! Parser for unified diffs, plain (`diff -u`) or git style, with one or many
//! files.

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

/// A file being collected.
struct Pending {
    old_path: Option<String>,
    new_path: Option<String>,
    /// Started by `diff --git`, so `---`/`+++` belong to it.
    git: bool,
    has_hunks: bool,
    binary: bool,
    builder: Builder,
}

impl Pending {
    fn new(old_path: Option<String>, new_path: Option<String>, git: bool) -> Self {
        Self { old_path, new_path, git, has_hunks: false, binary: false, builder: Builder::default() }
    }

    fn finish(self) -> FileDiff {
        self.builder.finish(self.old_path, self.new_path, self.binary)
    }
}

/// Lines still expected in the current hunk and the next line numbers.
#[derive(Copy, Clone)]
struct HunkState {
    old_left: u32,
    new_left: u32,
    old_no: u32,
    new_no: u32,
}

/// Parses a unified diff into one [`FileDiff`] per file. Text before the
/// first file header (a commit message, say) is skipped.
pub fn parse_unified(input: &str) -> Result<Vec<FileDiff>> {
    let mut files = Vec::new();
    let mut current: Option<Pending> = None;
    let mut hunk: Option<HunkState> = None;
    let mut last_line = 0;
    for (index, raw) in input.split_inclusive('\n').enumerate() {
        let number = index + 1;
        last_line = number;
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(state) = hunk.as_mut() {
            if state.old_left > 0 || state.new_left > 0 {
                let Some(file) = current.as_mut() else { return Err(ParseError::Orphan { line: number }) };
                hunk_line(file, state, line, number)?;
                continue;
            }
            hunk = None;
        }
        if line.starts_with('\\') {
            // "\ No newline at end of file"
            continue;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.extend(current.take().map(Pending::finish));
            let (old, new) = git_paths(rest);
            current = Some(Pending::new(Some(old.to_owned()), Some(new.to_owned()), true));
        } else if let Some(rest) = line.strip_prefix("--- ") {
            let path = header_path(rest, "a/");
            match current.as_mut() {
                Some(file) if file.git && !file.has_hunks => file.old_path = path,
                _ => {
                    files.extend(current.take().map(Pending::finish));
                    current = Some(Pending::new(path, None, false));
                }
            }
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            let file = current.as_mut().ok_or(ParseError::Orphan { line: number })?;
            file.new_path = header_path(rest, "b/");
        } else if let Some(rest) = line.strip_prefix("@@ ") {
            let file = current.as_mut().ok_or(ParseError::Orphan { line: number })?;
            let (state, section) = hunk_header(rest).ok_or(ParseError::HunkHeader { line: number })?;
            file.builder.hunk_break(section);
            file.has_hunks = true;
            hunk = Some(state);
        } else if let Some(file) = current.as_mut() {
            git_extended_header(file, line);
        }
    }
    if let Some(state) = hunk
        && (state.old_left > 0 || state.new_left > 0)
    {
        return Err(ParseError::Truncated { line: last_line, old: state.old_left, new: state.new_left });
    }
    files.extend(current.map(Pending::finish));
    Ok(files)
}

/// Adds one line of a hunk body.
fn hunk_line(file: &mut Pending, state: &mut HunkState, line: &str, number: usize) -> Result<()> {
    let overflow = ParseError::Overflow { line: number };
    let (marker, text) = match line.char_indices().nth(1) {
        Some((at, _)) => (&line[..at], &line[at..]),
        None => (line, ""),
    };
    match marker {
        // Some tools strip the space of empty context lines.
        " " | "" => {
            state.old_left = state.old_left.checked_sub(1).ok_or(overflow.clone())?;
            state.new_left = state.new_left.checked_sub(1).ok_or(overflow)?;
            file.builder.push(Tag::Equal, Some(state.old_no), Some(state.new_no), text);
            state.old_no += 1;
            state.new_no += 1;
        }
        "-" => {
            state.old_left = state.old_left.checked_sub(1).ok_or(overflow)?;
            file.builder.push(Tag::Delete, Some(state.old_no), None, text);
            state.old_no += 1;
        }
        "+" => {
            state.new_left = state.new_left.checked_sub(1).ok_or(overflow)?;
            file.builder.push(Tag::Insert, None, Some(state.new_no), text);
            state.new_no += 1;
        }
        // "\ No newline at end of file" sits between hunk lines too.
        "\\" => {}
        _ => return Err(ParseError::Truncated { line: number, old: state.old_left, new: state.new_left }),
    }
    Ok(())
}

/// Parses `-a[,b] +c[,d] @@ section` (the text after the opening `@@ `).
fn hunk_header(rest: &str) -> Option<(HunkState, &str)> {
    let (ranges, section) = rest.split_once("@@")?;
    let mut parts = ranges.split_whitespace();
    let (old_start, old_len) = range(parts.next()?.strip_prefix('-')?)?;
    let (new_start, new_len) = range(parts.next()?.strip_prefix('+')?)?;
    if parts.next().is_some() {
        return None;
    }
    // A zero length side names the line before the hunk.
    let first = |start: u32, len: u32| if len == 0 { start + 1 } else { start };
    let state = HunkState {
        old_left: old_len,
        new_left: new_len,
        old_no: first(old_start, old_len),
        new_no: first(new_start, new_len),
    };
    Some((state, section.trim_start()))
}

fn range(text: &str) -> Option<(u32, u32)> {
    match text.split_once(',') {
        Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

/// Path of a `---`/`+++` line: drops a trailing timestamp and the `a/` or `b/`
/// prefix. `/dev/null` means the file doesn't exist on that side.
fn header_path(rest: &str, prefix: &str) -> Option<String> {
    let path = rest.split('\t').next().unwrap_or(rest).trim_end();
    if path == "/dev/null" {
        return None;
    }
    Some(path.strip_prefix(prefix).unwrap_or(path).to_owned())
}

/// Paths of `diff --git a/old b/new`.
fn git_paths(rest: &str) -> (&str, &str) {
    let rest = rest.trim_end();
    match rest.strip_prefix("a/").and_then(|r| r.rsplit_once(" b/")) {
        Some((old, new)) => (old, new),
        None => rest.split_once(' ').unwrap_or((rest, rest)),
    }
}

/// Applies git's extended header lines between `diff --git` and the hunks.
fn git_extended_header(file: &mut Pending, line: &str) {
    if let Some(path) = line.strip_prefix("rename from ") {
        file.old_path = Some(path.to_owned());
    } else if let Some(path) = line.strip_prefix("rename to ") {
        file.new_path = Some(path.to_owned());
    } else if line.starts_with("new file mode") {
        file.old_path = None;
    } else if line.starts_with("deleted file mode") {
        file.new_path = None;
    } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
        file.binary = true;
    }
}
