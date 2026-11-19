//! Format aware diff of a native session against its export.
//!
//! JSON files are compared structurally (key order and whitespace don't
//! matter), JSONL files line by line with each line compared structurally,
//! anything else as text. Each file reports its first differences with a
//! JSON path, so a failing round trip names the field that changed.

use crate::{Error, Result};
use serde_json::Value as Json;
use similar::{Algorithm, DiffOp, TextDiff, capture_diff_slices};
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};

/// Differences listed per file before the rest are only counted.
pub const MAX_DIFFERENCES: usize = 20;

/// Longest value quoted in a difference.
const MAX_QUOTE: usize = 160;

/// How a file is compared, picked by extension.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Format {
    Json,
    /// `.jsonl` and `.ndjson`.
    JsonLines,
    Text,
}

impl Format {
    pub fn of(path: &Path) -> Format {
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => Format::Json,
            Some("jsonl" | "ndjson") => Format::JsonLines,
            _ => Format::Text,
        }
    }
}

/// The differences found in one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    /// Path relative to the compared roots; empty when the roots are files.
    pub path: PathBuf,
    pub format: Format,
    /// The first [`MAX_DIFFERENCES`] differences, readable on their own.
    pub differences: Vec<String>,
    /// Differences found past the listed ones.
    pub more: usize,
}

impl fmt::Display for FileDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = if self.path.as_os_str().is_empty() { Path::new("<file>") } else { &self.path };
        writeln!(f, "{} ({:?}):", name.display(), self.format)?;
        for d in &self.differences {
            for line in d.lines() {
                writeln!(f, "    {line}")?;
            }
        }
        if self.more > 0 {
            writeln!(f, "    … and {} more", self.more)?;
        }
        Ok(())
    }
}

/// Collects differences up to the limit and counts the rest.
struct Sink {
    differences: Vec<String>,
    more: usize,
}

impl Sink {
    fn new() -> Self {
        Self { differences: Vec::new(), more: 0 }
    }

    fn push(&mut self, difference: impl FnOnce() -> String) {
        if self.differences.len() < MAX_DIFFERENCES {
            self.differences.push(difference());
        } else {
            self.more += 1;
        }
    }

    fn into_diff(self, path: PathBuf, format: Format) -> Option<FileDiff> {
        (!self.differences.is_empty()).then_some(FileDiff { path, format, differences: self.differences, more: self.more })
    }
}

/// Compares `expected` with `actual`, two files or two directories. Empty
/// when they match.
pub fn diff_paths(expected: &Path, actual: &Path) -> Result<Vec<FileDiff>> {
    let meta = std::fs::metadata(expected).map_err(|source| Error::io(expected, source))?;
    if !meta.is_dir() {
        let format = Format::of(expected);
        let a = read(expected)?;
        let b = read(actual)?;
        return Ok(diff_bytes(format, &a, &b).map(|mut d| {
            d.path = PathBuf::new();
            d
        }).into_iter().collect());
    }
    let mut left = Vec::new();
    walk(expected, Path::new(""), &mut left)?;
    let mut right = Vec::new();
    if actual.is_dir() {
        walk(actual, Path::new(""), &mut right)?;
    }
    left.sort();
    right.sort();
    let mut out = Vec::new();
    for rel in &left {
        if right.binary_search(rel).is_err() {
            out.push(only(rel, "missing from the export"));
            continue;
        }
        let format = Format::of(rel);
        let a = read(&expected.join(rel))?;
        let b = read(&actual.join(rel))?;
        if let Some(mut d) = diff_bytes(format, &a, &b) {
            d.path.clone_from(rel);
            out.push(d);
        }
    }
    for rel in right.iter().filter(|r| left.binary_search(r).is_err()) {
        out.push(only(rel, "only in the export"));
    }
    Ok(out)
}

fn only(rel: &Path, what: &str) -> FileDiff {
    FileDiff { path: rel.to_owned(), format: Format::of(rel), differences: vec![what.to_owned()], more: 0 }
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|source| Error::io(path, source))
}

fn walk(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let dir = root.join(rel);
    for entry in std::fs::read_dir(&dir).map_err(|source| Error::io(&dir, source))? {
        let entry = entry.map_err(|source| Error::io(&dir, source))?;
        let rel = rel.join(entry.file_name());
        if entry.file_type().map_err(|source| Error::io(&dir, source))?.is_dir() {
            walk(root, &rel, out)?;
        } else {
            out.push(rel);
        }
    }
    Ok(())
}

/// Compares two file contents as `format`. `None` when they match.
pub fn diff_bytes(format: Format, expected: &[u8], actual: &[u8]) -> Option<FileDiff> {
    let mut sink = Sink::new();
    match (std::str::from_utf8(expected), std::str::from_utf8(actual)) {
        (Ok(a), Ok(b)) => match format {
            Format::Json => diff_json_text(a, b, "$", &mut sink),
            Format::JsonLines => diff_jsonl(a, b, &mut sink),
            Format::Text => diff_text(a, b, &mut sink),
        },
        _ if expected == actual => {}
        _ => sink.push(|| "binary contents differ".to_owned()),
    }
    sink.into_diff(PathBuf::new(), format)
}

fn diff_json_text(a: &str, b: &str, at: &str, sink: &mut Sink) {
    let left: Json = match serde_json::from_str(a) {
        Ok(v) => v,
        // The original isn't JSON either: fall back to comparing text.
        Err(_) => return diff_text(a, b, sink),
    };
    match serde_json::from_str::<Json>(b) {
        Ok(right) => {
            let mut path = at.to_owned();
            diff_json(&mut path, &left, &right, sink);
        }
        Err(err) => sink.push(|| format!("{at}: the export isn't JSON: {err}")),
    }
}

/// Aligns the lines first, so one lost record reads as one missing line
/// instead of shifting every line after it, then compares paired lines
/// structurally.
fn diff_jsonl(a: &str, b: &str, sink: &mut Sink) {
    let left = numbered(a);
    let right = numbered(b);
    let left_keys: Vec<String> = left.iter().map(|(_, l)| canonical(l)).collect();
    let right_keys: Vec<String> = right.iter().map(|(_, l)| canonical(l)).collect();
    for op in capture_diff_slices(Algorithm::Myers, &left_keys, &right_keys) {
        let (old, new) = match op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Delete { old_index, old_len, .. } => (old_index..old_index + old_len, 0..0),
            DiffOp::Insert { new_index, new_len, .. } => (0..0, new_index..new_index + new_len),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                (old_index..old_index + old_len, new_index..new_index + new_len)
            }
        };
        let paired = old.len().min(new.len());
        for (i, j) in old.clone().zip(new.clone()) {
            let ((n, a), (m, b)) = (left[i], right[j]);
            let at = if n == m { format!("line {n} $") } else { format!("line {n} (export line {m}) $") };
            diff_json_text(a, b, &at, sink);
        }
        for (n, a) in old.skip(paired).map(|i| left[i]) {
            sink.push(|| format!("line {n}: missing from the export: {}", quote(a)));
        }
        for (m, b) in new.skip(paired).map(|j| right[j]) {
            sink.push(|| format!("export line {m}: not in the original: {}", quote(b)));
        }
    }
}

/// A line's JSON with sorted keys and no whitespace, or the line itself
/// when it isn't JSON.
fn canonical(line: &str) -> String {
    serde_json::from_str::<Json>(line).map_or_else(|_| line.to_owned(), |v| v.to_string())
}

/// Non-blank lines with their 1-based numbers.
fn numbered(text: &str) -> Vec<(usize, &str)> {
    text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()).map(|(i, l)| (i + 1, l)).collect()
}

fn diff_text(a: &str, b: &str, sink: &mut Sink) {
    if a == b {
        return;
    }
    let diff = TextDiff::from_lines(a, b);
    for hunk in diff.unified_diff().context_radius(2).iter_hunks() {
        sink.push(|| hunk.to_string());
    }
}

fn diff_json(path: &mut String, a: &Json, b: &Json, sink: &mut Sink) {
    match (a, b) {
        (Json::Object(x), Json::Object(y)) => {
            let mut original_keys: Vec<_> = x.keys().collect();
            original_keys.sort_unstable();
            for key in original_keys {
                let va = &x[key];
                let len = path.len();
                push_key(path, key);
                match y.get(key) {
                    Some(vb) => diff_json(path, va, vb, sink),
                    None => sink.push(|| format!("{path}: missing from the export (was {})", quote_json(va))),
                }
                path.truncate(len);
            }
            let mut added_keys: Vec<_> = y.keys().filter(|key| !x.contains_key(*key)).collect();
            added_keys.sort_unstable();
            for key in added_keys {
                let vb = &y[key];
                let len = path.len();
                push_key(path, key);
                sink.push(|| format!("{path}: only in the export ({})", quote_json(vb)));
                path.truncate(len);
            }
        }
        (Json::Array(x), Json::Array(y)) => {
            for (i, (va, vb)) in x.iter().zip(y).enumerate() {
                let len = path.len();
                let _ = write!(path, "[{i}]");
                diff_json(path, va, vb, sink);
                path.truncate(len);
            }
            if x.len() != y.len() {
                sink.push(|| format!("{path}: {} items in the original, {} in the export", x.len(), y.len()));
            }
        }
        _ if a == b => {}
        _ => sink.push(|| format!("{path}: expected {}, got {}", quote_json(a), quote_json(b))),
    }
}

fn push_key(path: &mut String, key: &str) {
    if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        path.push('.');
        path.push_str(key);
    } else {
        let _ = write!(path, "[{}]", Json::from(key));
    }
}

fn quote_json(v: &Json) -> String {
    quote(&v.to_string())
}

fn quote(text: &str) -> String {
    match text.char_indices().nth(MAX_QUOTE) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}
