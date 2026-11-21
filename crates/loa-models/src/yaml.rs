//! Comment preserving editor for YAML files made of block mappings, such as omp's
//! `config.yml` and `models.yml`.
//!
//! No Rust YAML crate keeps comments on a round trip, so edits are made on the
//! document text: the editor finds the touched key's lines, rewrites only those
//! lines, and leaves every other byte alone. Each edit is checked: the new text
//! is parsed and must equal the old document with exactly the intended change,
//! otherwise the edit is refused and the document is unchanged.

use yaml_rust2::YamlLoader;
use yaml_rust2::yaml::Hash;

pub use yaml_rust2::Yaml;

use crate::{Error, Result};

/// A YAML document whose root is a block mapping (or empty), edited in place.
#[derive(Debug, Clone)]
pub struct YamlDocument {
    text: String,
    root: Yaml,
    what: String,
}

impl YamlDocument {
    /// Parses `text`. `what` names the document in errors (usually its path).
    pub fn parse(text: &str, what: &str) -> Result<Self> {
        let root = load_root(text, what)?;
        Ok(Self {
            text: text.to_owned(),
            root,
            what: what.to_owned(),
        })
    }

    /// The current document text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The parsed root mapping.
    pub fn root(&self) -> &Yaml {
        &self.root
    }

    /// The value at `path` (a key per mapping level), if present.
    pub fn get(&self, path: &[&str]) -> Option<&Yaml> {
        get_path(&self.root, path)
    }

    /// Sets the value at `path`, creating missing parent mappings. Only the lines
    /// of the touched key change.
    pub fn set(&mut self, path: &[&str], value: Yaml) -> Result<()> {
        if path.is_empty() {
            return Err(Error::invalid("path", "empty key path"));
        }
        let mut expected = self.root.clone();
        model_set(&mut expected, path, value, &self.what)?;
        if expected == self.root {
            return Ok(());
        }
        let text = Editor::new(&self.text, &self.what).set(path, &expected)?;
        self.commit(text, expected)
    }

    /// Removes the key at `path`. A parent mapping left empty is removed too, so
    /// no empty `key:` (which YAML reads as null) is left behind. Returns whether
    /// the key existed.
    pub fn remove(&mut self, path: &[&str]) -> Result<bool> {
        if get_path(&self.root, path).is_none() {
            return Ok(false);
        }
        let mut expected = self.root.clone();
        let removed_depth = model_remove(&mut expected, path);
        let text = Editor::new(&self.text, &self.what).remove(&path[..=removed_depth])?;
        self.commit(text, expected)?;
        Ok(true)
    }

    fn commit(&mut self, text: String, expected: Yaml) -> Result<()> {
        let parsed = load_root(&text, &self.what).map_err(|_| Error::EditCheck {
            path: self.what.clone(),
        })?;
        if parsed != expected {
            return Err(Error::EditCheck {
                path: self.what.clone(),
            });
        }
        self.text = text;
        self.root = parsed;
        Ok(())
    }
}

fn load_root(text: &str, what: &str) -> Result<Yaml> {
    let docs = YamlLoader::load_from_str(text).map_err(|e| Error::Parse {
        what: what.to_owned(),
        message: e.to_string(),
    })?;
    let mut docs = docs.into_iter();
    let root = match (docs.next(), docs.next()) {
        (None, _) => return Ok(Yaml::Hash(Hash::new())),
        (Some(_), Some(_)) => {
            return Err(Error::Unsupported {
                what: what.to_owned(),
                reason: "it holds several YAML documents".into(),
            });
        }
        (Some(root), None) => root,
    };
    match root {
        Yaml::Null => Ok(Yaml::Hash(Hash::new())),
        Yaml::Hash(_) => {
            if first_content_line(text).is_some_and(|l| l.trim_start().starts_with('{')) {
                return Err(Error::Unsupported {
                    what: what.to_owned(),
                    reason: "its root is a flow mapping".into(),
                });
            }
            Ok(root)
        }
        _ => Err(Error::Unsupported {
            what: what.to_owned(),
            reason: "its root is not a mapping".into(),
        }),
    }
}

fn first_content_line(text: &str) -> Option<&str> {
    text.lines()
        .find(|l| is_content(l) && !is_doc_marker(l))
}

/// Looks up `path` in nested mappings.
pub fn get_path<'a>(root: &'a Yaml, path: &[&str]) -> Option<&'a Yaml> {
    let mut node = root;
    for key in path {
        node = node.as_hash()?.get(&Yaml::String((*key).to_owned()))?;
    }
    Some(node)
}

fn model_set(root: &mut Yaml, path: &[&str], value: Yaml, what: &str) -> Result<()> {
    let mut node = root;
    for (depth, key) in path.iter().enumerate() {
        if node.is_null() {
            *node = Yaml::Hash(Hash::new());
        }
        let Yaml::Hash(map) = node else {
            return Err(Error::Unsupported {
                what: what.to_owned(),
                reason: format!("`{}` is not a mapping", path[..depth].join(".")),
            });
        };
        let k = Yaml::String((*key).to_owned());
        if depth + 1 == path.len() {
            map.replace(k, value);
            return Ok(());
        }
        // hashlink's `or_insert_with` moves an existing entry to the back.
        // The parsed YAML mapping is ordered, so preserve its position here.
        if !map.contains_key(&k) {
            map.replace(k.clone(), Yaml::Hash(Hash::new()));
        }
        node = map.get_mut(&k).expect("parent mapping key was just inserted");
    }
    Ok(())
}

/// Removes `path` from the model, pruning parents left empty. Returns the depth
/// of the outermost removed key.
fn model_remove(root: &mut Yaml, path: &[&str]) -> usize {
    fn go(node: &mut Yaml, path: &[&str], depth: usize) -> Option<usize> {
        let map = node.as_mut_hash()?;
        let k = Yaml::String(path[depth].to_owned());
        if depth + 1 == path.len() {
            map.remove(&k);
            return Some(depth);
        }
        let child = map.get_mut(&k)?;
        let removed = go(child, path, depth + 1)?;
        if child.as_hash().is_some_and(|h| h.is_empty()) {
            map.remove(&k);
            return Some(depth);
        }
        Some(removed)
    }
    go(root, path, 0).unwrap_or(path.len() - 1)
}

fn is_content(line: &str) -> bool {
    let t = line.trim_start_matches(' ');
    !t.trim().is_empty() && !t.starts_with('#')
}

fn is_doc_marker(line: &str) -> bool {
    let t = line.trim_end();
    t == "---" || t == "..." || t.starts_with("--- #") || t.starts_with("... #")
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn is_seq_item(line: &str) -> bool {
    let t = line.trim_start_matches(' ');
    t == "-" || t.starts_with("- ") || t.trim_end() == "-"
}

/// One physical line: byte range of its text (without the line break) and the
/// offset where the next line starts.
#[derive(Debug, Clone, Copy)]
struct LineSpan {
    start: usize,
    end: usize,
    next: usize,
}

/// A key line found in a mapping and the lines its value occupies.
#[derive(Debug, Clone)]
struct Entry {
    line: usize,
    indent: usize,
    /// Byte length (within the line) of the indent, key and colon.
    head_len: usize,
    /// The value written on the key line, without comment, trimmed.
    inline: String,
    /// The comment on the key line, from `#`.
    comment: Option<String>,
    /// One past the last line belonging to the value.
    body_end: usize,
}

/// A range of lines holding one block mapping.
#[derive(Debug, Clone, Copy)]
struct Region {
    start: usize,
    end: usize,
}

struct Editor<'a> {
    text: &'a str,
    what: &'a str,
    lines: Vec<LineSpan>,
    newline: &'static str,
    unit: usize,
}

impl<'a> Editor<'a> {
    fn new(text: &'a str, what: &'a str) -> Self {
        let mut lines = Vec::new();
        let mut start = 0;
        let bytes = text.as_bytes();
        while start < text.len() {
            let nl = text[start..].find('\n').map(|i| start + i);
            let (end, next) = match nl {
                Some(i) => (if i > start && bytes[i - 1] == b'\r' { i - 1 } else { i }, i + 1),
                None => (text.len(), text.len()),
            };
            lines.push(LineSpan { start, end, next });
            start = next;
        }
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let mut editor = Self {
            text,
            what,
            lines,
            newline,
            unit: 2,
        };
        editor.unit = editor.detect_unit();
        editor
    }

    fn line(&self, i: usize) -> &'a str {
        let span = self.lines[i];
        &self.text[span.start..span.end]
    }

    fn detect_unit(&self) -> usize {
        let mut prev: Option<usize> = None;
        for i in 0..self.lines.len() {
            let l = self.line(i);
            if !is_content(l) || is_doc_marker(l) {
                continue;
            }
            let ind = indent_of(l);
            if let Some(p) = prev
                && ind > p
            {
                return ind - p;
            }
            prev = Some(ind);
        }
        2
    }

    fn unsupported(&self, reason: impl Into<String>) -> Error {
        Error::Unsupported {
            what: self.what.to_owned(),
            reason: reason.into(),
        }
    }

    fn root_region(&self) -> Region {
        Region {
            start: 0,
            end: self.lines.len(),
        }
    }

    /// Indent of the keys of the mapping in `region`, or `None` when empty.
    fn key_indent(&self, region: Region) -> Option<usize> {
        (region.start..region.end)
            .map(|i| self.line(i))
            .find(|l| is_content(l) && !is_doc_marker(l))
            .map(indent_of)
    }

    /// The entries of the mapping in `region`, in order.
    fn entries(&self, region: Region) -> Result<Vec<(String, Entry)>> {
        let Some(key_indent) = self.key_indent(region) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut i = region.start;
        while i < region.end {
            let l = self.line(i);
            if !is_content(l) || is_doc_marker(l) {
                i += 1;
                continue;
            }
            if indent_of(l) != key_indent || is_seq_item(l) {
                return Err(self.unsupported(format!("line {} is not a mapping key", i + 1)));
            }
            let (key, head_len) = parse_key(l, key_indent)
                .ok_or_else(|| self.unsupported(format!("line {} has a key form the editor does not handle", i + 1)))?;
            let (inline, comment) = split_comment(&l[head_len..]);
            let block_scalar = inline.starts_with('|') || inline.starts_with('>');
            let mut body_end = i + 1;
            let mut j = i + 1;
            while j < region.end {
                let lj = self.line(j);
                let ind = indent_of(lj);
                if lj.trim().is_empty() {
                    j += 1;
                    continue;
                }
                if block_scalar {
                    if ind > key_indent {
                        body_end = j + 1;
                        j += 1;
                        continue;
                    }
                    if !is_content(lj) {
                        j += 1;
                        continue;
                    }
                    break;
                }
                if !is_content(lj) {
                    j += 1;
                    continue;
                }
                if ind > key_indent || (ind == key_indent && is_seq_item(lj)) {
                    body_end = j + 1;
                    j += 1;
                    continue;
                }
                break;
            }
            out.push((
                key,
                Entry {
                    line: i,
                    indent: key_indent,
                    head_len,
                    inline: inline.to_owned(),
                    comment: comment.map(str::to_owned),
                    body_end,
                },
            ));
            i = body_end;
        }
        Ok(out)
    }

    fn find(&self, region: Region, key: &str) -> Result<Option<Entry>> {
        Ok(self
            .entries(region)?
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, e)| e))
    }

    /// The region of a block mapping value, when `entry` holds one.
    fn child_mapping(&self, entry: &Entry) -> Option<Region> {
        if !entry.inline.is_empty() || entry.body_end <= entry.line + 1 {
            return None;
        }
        let region = Region {
            start: entry.line + 1,
            end: entry.body_end,
        };
        let first = (region.start..region.end)
            .map(|i| self.line(i))
            .find(|l| is_content(l))?;
        if is_seq_item(first) || indent_of(first) <= entry.indent {
            return None;
        }
        Some(region)
    }

    fn set(&self, path: &[&str], expected_root: &Yaml) -> Result<String> {
        let mut region = self.root_region();
        let mut is_root = true;
        for depth in 0..path.len() {
            let sub = get_path(expected_root, &path[..=depth])
                .ok_or_else(|| self.unsupported("edited key missing from the result"))?;
            match self.find(region, path[depth])? {
                Some(entry) => {
                    if depth + 1 < path.len()
                        && let Some(child) = self.child_mapping(&entry)
                    {
                        region = child;
                        is_root = false;
                        continue;
                    }
                    if depth + 1 < path.len() && !entry.inline.is_empty() {
                        return Err(self.unsupported(format!(
                            "`{}` is written inline (flow style); editing it would reformat its line, so edit it by hand",
                            path[..=depth].join(".")
                        )));
                    }
                    return Ok(self.replace_value(&entry, sub));
                }
                None => return self.insert_entry(region, is_root, path[depth], sub),
            }
        }
        Err(self.unsupported("empty key path"))
    }

    fn remove(&self, path: &[&str]) -> Result<String> {
        let mut region = self.root_region();
        for depth in 0..path.len() {
            let entry = self
                .find(region, path[depth])?
                .ok_or_else(|| self.unsupported(format!("`{}` not found in the text", path[..=depth].join("."))))?;
            if depth + 1 == path.len() {
                let end = self.span_end(entry.body_end);
                let start = self.lines[entry.line].start;
                let mut out = String::with_capacity(self.text.len());
                out.push_str(&self.text[..start]);
                out.push_str(&self.text[end..]);
                return Ok(out);
            }
            region = self
                .child_mapping(&entry)
                .ok_or_else(|| self.unsupported(format!("`{}` is not a block mapping", path[..=depth].join("."))))?;
        }
        Err(self.unsupported("empty key path"))
    }

    /// Byte offset where line `i` starts, or the text end.
    fn span_end(&self, i: usize) -> usize {
        if i == 0 {
            0
        } else {
            self.lines[i - 1].next
        }
    }

    fn replace_value(&self, entry: &Entry, value: &Yaml) -> String {
        let line = self.line(entry.line);
        let head = &line[..entry.head_len];
        let mut new_lines = Vec::new();
        let comment = entry.comment.as_deref().map(|c| format!(" {c}")).unwrap_or_default();
        match render_inline(value) {
            Some(inline) => new_lines.push(format!("{head} {inline}{comment}")),
            None => {
                new_lines.push(format!("{head}{comment}"));
                render_block(value, entry.indent + self.unit, self.unit, &mut new_lines);
            }
        }
        let start = self.lines[entry.line].start;
        let end = self.span_end(entry.body_end);
        let had_newline = self.lines[entry.body_end - 1].next > self.lines[entry.body_end - 1].end;
        self.splice(start, end, &new_lines, had_newline)
    }

    fn insert_entry(&self, region: Region, is_root: bool, key: &str, value: &Yaml) -> Result<String> {
        let indent = self.key_indent(region).unwrap_or(0);
        let mut new_lines = Vec::new();
        render_entry(key, value, indent, self.unit, &mut new_lines);
        let entries = self.entries(region)?;
        let at = match entries.last() {
            // After the body of the last entry of this mapping, before any
            // trailing comments.
            Some((_, last)) => self.span_end(last.body_end),
            None if is_root => self.text.len(),
            None => self.span_end(region.end),
        };
        let mut out = String::with_capacity(self.text.len() + 64);
        out.push_str(&self.text[..at]);
        if at > 0 && !self.text[..at].ends_with('\n') {
            out.push_str(self.newline);
        }
        for l in &new_lines {
            out.push_str(l);
            out.push_str(self.newline);
        }
        out.push_str(&self.text[at..]);
        Ok(out)
    }

    fn splice(&self, start: usize, end: usize, new_lines: &[String], trailing_newline: bool) -> String {
        let mut out = String::with_capacity(self.text.len() + 64);
        out.push_str(&self.text[..start]);
        for (n, l) in new_lines.iter().enumerate() {
            out.push_str(l);
            if n + 1 < new_lines.len() || trailing_newline {
                out.push_str(self.newline);
            }
        }
        out.push_str(&self.text[end..]);
        out
    }
}

/// Parses a mapping key at the start of `line` (after `indent` spaces). Returns
/// the decoded key and the byte length up to and including the colon.
fn parse_key(line: &str, indent: usize) -> Option<(String, usize)> {
    let rest = &line[indent..];
    let (key, consumed) = match rest.as_bytes().first()? {
        b'"' => {
            let mut escaped = false;
            let mut close = None;
            for (i, c) in rest.char_indices().skip(1) {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    close = Some(i);
                    break;
                }
            }
            let close = close?;
            let key: String = serde_json::from_str(&rest[..=close]).ok()?;
            (key, close + 1)
        }
        b'\'' => {
            let bytes = rest.as_bytes();
            let mut i = 1;
            let mut key = String::new();
            loop {
                let c = rest[i..].chars().next()?;
                if c == '\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        key.push('\'');
                        i += 2;
                        continue;
                    }
                    break;
                }
                key.push(c);
                i += c.len_utf8();
            }
            (key, i + 1)
        }
        b'?' | b'[' | b'{' | b'&' | b'*' | b'!' | b'|' | b'>' | b'%' | b'@' | b'`' => return None,
        b'-' if rest == "-" || rest.starts_with("- ") => return None,
        _ => plain_key(rest)?,
    };
    let after = &rest[consumed..];
    let colon = after.find(|c: char| c != ' ')?;
    if after.as_bytes()[colon] != b':' {
        return None;
    }
    let tail = &after[colon + 1..];
    if !(tail.is_empty() || tail.starts_with(' ') || tail.starts_with('\t')) {
        return None;
    }
    Some((key, indent + consumed + colon + 1))
}

fn plain_key(rest: &str) -> Option<(String, usize)> {
    let bytes = rest.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b':' && (i + 1 == bytes.len() || bytes[i + 1] == b' ' || bytes[i + 1] == b'\t') {
            let key = rest[..i].trim_end();
            if key.is_empty() || key.contains(" #") {
                return None;
            }
            return Some((key.to_owned(), key.len()));
        }
        if b == b'#' && i > 0 && bytes[i - 1] == b' ' {
            return None;
        }
    }
    None
}

/// Splits the text after a key's colon into the value and the trailing comment.
fn split_comment(rest: &str) -> (&str, Option<&str>) {
    let bytes = rest.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(b'"') if b == b'\\' => i += 1,
            Some(q) if b == q => {
                if q == b'\'' && bytes.get(i + 1) == Some(&b'\'') {
                    i += 1;
                } else {
                    quote = None;
                }
            }
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'#' && (i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') => {
                return (rest[..i].trim(), Some(rest[i..].trim_end()));
            }
            None => {}
        }
        i += 1;
    }
    (rest.trim(), None)
}

/// Renders a value that fits on the key line, or `None` for block values.
fn render_inline(value: &Yaml) -> Option<String> {
    match value {
        Yaml::String(s) => Some(render_string(s)),
        Yaml::Integer(n) => Some(n.to_string()),
        Yaml::Real(r) => Some(r.clone()),
        Yaml::Boolean(b) => Some(b.to_string()),
        Yaml::Null | Yaml::BadValue | Yaml::Alias(_) => Some("null".into()),
        Yaml::Array(a) if a.is_empty() => Some("[]".into()),
        Yaml::Hash(h) if h.is_empty() => Some("{}".into()),
        Yaml::Array(_) | Yaml::Hash(_) => None,
    }
}

fn render_key(key: &Yaml) -> String {
    match key {
        Yaml::String(s) => render_string(s),
        other => render_inline(other).unwrap_or_else(|| "null".into()),
    }
}

fn render_entry(key: &str, value: &Yaml, indent: usize, unit: usize, out: &mut Vec<String>) {
    let pad = " ".repeat(indent);
    let key = render_string(key);
    match render_inline(value) {
        Some(v) => out.push(format!("{pad}{key}: {v}")),
        None => {
            out.push(format!("{pad}{key}:"));
            render_block(value, indent + unit, unit, out);
        }
    }
}

/// Renders a mapping or sequence as block lines at `indent`.
fn render_block(value: &Yaml, indent: usize, unit: usize, out: &mut Vec<String>) {
    let pad = " ".repeat(indent);
    match value {
        Yaml::Hash(map) => {
            for (k, v) in map {
                let key = render_key(k);
                match render_inline(v) {
                    Some(s) => out.push(format!("{pad}{key}: {s}")),
                    None => {
                        out.push(format!("{pad}{key}:"));
                        render_block(v, indent + unit, unit, out);
                    }
                }
            }
        }
        Yaml::Array(items) => {
            for item in items {
                match render_inline(item) {
                    Some(s) => out.push(format!("{pad}- {s}")),
                    None => {
                        let mut inner = Vec::new();
                        render_block(item, indent + 2, unit, &mut inner);
                        let mut iter = inner.into_iter();
                        if let Some(first) = iter.next() {
                            out.push(format!("{pad}- {}", &first[indent + 2..]));
                        }
                        out.extend(iter);
                    }
                }
            }
        }
        other => {
            if let Some(s) = render_inline(other) {
                out.push(format!("{pad}{s}"));
            }
        }
    }
}

/// Renders a string as a plain scalar when that reads back as the same string,
/// otherwise double quoted.
fn render_string(s: &str) -> String {
    if plain_safe(s) {
        s.to_owned()
    } else {
        // A JSON string literal is a valid YAML double quoted scalar.
        serde_json::Value::String(s.to_owned()).to_string()
    }
}

fn plain_safe(s: &str) -> bool {
    let Some(first) = s.chars().next() else {
        return false;
    };
    if !(first.is_ascii_alphanumeric() || matches!(first, '_' | '.' | '/' | '$' | '~')) {
        return false;
    }
    if s.ends_with([':', ' ']) || s.contains(": ") || s.contains(" #") || s.contains("  ") {
        return false;
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '@' | '+' | '$' | '~' | ':' | '(' | ')' | ' ' | '*' | ','))
    {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "y" | "n" | "yes" | "no" | "on" | "off" | "true" | "false" | "null" | "~"
    ) {
        return false;
    }
    let probe = format!("k: {s}\n");
    matches!(
        YamlLoader::load_from_str(&probe).ok().and_then(|d| d.into_iter().next()),
        Some(Yaml::Hash(h)) if h.get(&Yaml::String("k".into())) == Some(&Yaml::String(s.to_owned()))
    )
}

/// Builds a YAML string.
pub fn ystr(s: impl Into<String>) -> Yaml {
    Yaml::String(s.into())
}

/// Builds a YAML sequence of strings.
pub fn ystrs<I, S>(items: I) -> Yaml
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    Yaml::Array(items.into_iter().map(|s| Yaml::String(s.into())).collect())
}

/// Builds a YAML mapping from key/value pairs, in order.
pub fn ymap<I, K>(pairs: I) -> Yaml
where
    I: IntoIterator<Item = (K, Yaml)>,
    K: Into<String>,
{
    let mut map = Hash::new();
    for (k, v) in pairs {
        map.replace(Yaml::String(k.into()), v);
    }
    Yaml::Hash(map)
}

/// Converts a YAML value to JSON (for comparing with omp's JSON output).
pub fn to_json(value: &Yaml) -> serde_json::Value {
    use serde_json::Value as J;
    match value {
        Yaml::String(s) => J::String(s.clone()),
        Yaml::Integer(n) => J::from(*n),
        Yaml::Real(r) => r
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or(J::Null, J::Number),
        Yaml::Boolean(b) => J::Bool(*b),
        Yaml::Array(a) => J::Array(a.iter().map(to_json).collect()),
        Yaml::Hash(h) => J::Object(
            h.iter()
                .map(|(k, v)| {
                    let key = match k {
                        Yaml::String(s) => s.clone(),
                        other => render_key(other),
                    };
                    (key, to_json(v))
                })
                .collect(),
        ),
        Yaml::Null | Yaml::BadValue | Yaml::Alias(_) => J::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "\
# omp settings, edited by hand
theme:
  dark: titanium   # favourite
  light: light

modelRoles:
  default: anthropic/claude-sonnet-4-5 # main model
  # smol: openai/gpt-4.1-mini
  slow: \"anthropic/claude-opus-4-5:high\"

enabledModels:
  - claude-sonnet-4-5
  - path: ~/work
    models:
      - anthropic/claude-opus-4-5

# trailing note
";

    fn doc(text: &str) -> YamlDocument {
        YamlDocument::parse(text, "config.yml").unwrap()
    }

    #[test]
    fn setting_existing_scalar_changes_only_that_line_and_keeps_its_comment() {
        let mut d = doc(CONFIG);
        d.set(&["modelRoles", "default"], ystr("openai/gpt-5")).unwrap();
        let expected = CONFIG.replace(
            "  default: anthropic/claude-sonnet-4-5 # main model",
            "  default: openai/gpt-5 # main model",
        );
        assert_eq!(d.text(), expected);
    }

    #[test]
    fn adding_a_key_to_a_nested_mapping_appends_after_its_last_entry() {
        let mut d = doc(CONFIG);
        d.set(&["modelRoles", "plan"], ystr("@slow")).unwrap();
        let expected = CONFIG.replace(
            "  slow: \"anthropic/claude-opus-4-5:high\"\n",
            "  slow: \"anthropic/claude-opus-4-5:high\"\n  plan: \"@slow\"\n",
        );
        assert_eq!(d.text(), expected);
        assert_eq!(d.get(&["modelRoles", "plan"]), Some(&ystr("@slow")));
    }

    #[test]
    fn adding_a_nested_path_creates_parents_at_the_end_of_the_root() {
        let mut d = doc(CONFIG);
        d.set(&["retry", "fallbackChains", "default"], ystrs(["openai/gpt-5", "@smol"]))
            .unwrap();
        assert!(d.text().starts_with(CONFIG.trim_end_matches("\n# trailing note\n")));
        assert!(d.text().contains(
            "retry:\n  fallbackChains:\n    default:\n      - openai/gpt-5\n      - \"@smol\"\n"
        ));
        assert!(d.text().contains("# omp settings, edited by hand"));
        assert!(d.text().contains("# smol: openai/gpt-4.1-mini"));
    }

    #[test]
    fn replacing_a_list_keeps_neighbouring_comments_and_keys() {
        let mut d = doc(CONFIG);
        d.set(&["enabledModels"], ystrs(["gpt-5"])).unwrap();
        let expected = "\
# omp settings, edited by hand
theme:
  dark: titanium   # favourite
  light: light

modelRoles:
  default: anthropic/claude-sonnet-4-5 # main model
  # smol: openai/gpt-4.1-mini
  slow: \"anthropic/claude-opus-4-5:high\"

enabledModels:
  - gpt-5

# trailing note
";
        assert_eq!(d.text(), expected);
    }

    #[test]
    fn removing_the_last_child_removes_the_empty_parent() {
        let text = "a: 1\nretry:\n  fallbackChains:\n    smol: [x]\nb: 2\n";
        let mut d = doc(text);
        assert!(d.remove(&["retry", "fallbackChains", "smol"]).unwrap());
        assert_eq!(d.text(), "a: 1\nb: 2\n");
        assert!(!d.remove(&["retry"]).unwrap());
    }

    #[test]
    fn removing_a_middle_key_keeps_the_rest() {
        let mut d = doc(CONFIG);
        d.remove(&["modelRoles", "slow"]).unwrap();
        assert!(d.text().contains(
            "modelRoles:\n  default: anthropic/claude-sonnet-4-5 # main model\n  # smol: openai/gpt-4.1-mini\n\nenabledModels:"
        ));
    }

    #[test]
    fn three_space_indent_is_kept_for_new_keys() {
        let text = "providers:\n   my-gw:\n      baseUrl: http://x\n";
        let mut d = doc(text);
        d.set(&["providers", "my-gw", "modelOverrides", "m1", "name"], ystr("M One"))
            .unwrap();
        assert_eq!(
            d.text(),
            "providers:\n   my-gw:\n      baseUrl: http://x\n      modelOverrides:\n         m1:\n            name: M One\n"
        );
    }

    #[test]
    fn null_parent_becomes_a_block_mapping() {
        let mut d = doc("modelRoles:\nother: 1\n");
        d.set(&["modelRoles", "default"], ystr("a/b")).unwrap();
        assert_eq!(d.text(), "modelRoles:\n  default: a/b\nother: 1\n");
    }

    #[test]
    fn empty_and_comment_only_files_accept_keys() {
        let mut d = doc("");
        d.set(&["modelProviderOrder"], ystrs(["anthropic"])).unwrap();
        assert_eq!(d.text(), "modelProviderOrder:\n  - anthropic\n");

        let mut d = doc("# nothing yet");
        d.set(&["a"], ystr("b")).unwrap();
        assert_eq!(d.text(), "# nothing yet\na: b\n");
    }

    #[test]
    fn values_that_yaml_would_misread_are_quoted() {
        let mut d = doc("");
        for (k, v) in [("a", "yes"), ("b", "1.5"), ("c", "@smol"), ("d", "x: y"), ("e", "!op read")] {
            d.set(&[k], ystr(v)).unwrap();
        }
        for (k, v) in [("a", "yes"), ("b", "1.5"), ("c", "@smol"), ("d", "x: y"), ("e", "!op read")] {
            assert_eq!(d.get(&[k]), Some(&ystr(v)));
        }
        assert!(d.text().contains("e: \"!op read\""));
    }

    #[test]
    fn crlf_files_stay_crlf() {
        let mut d = doc("a: 1\r\nb:\r\n  c: 2\r\n");
        d.set(&["b", "d"], ystr("x")).unwrap();
        assert_eq!(d.text(), "a: 1\r\nb:\r\n  c: 2\r\n  d: x\r\n");
    }

    #[test]
    fn block_scalar_bodies_with_hash_lines_are_replaced_whole() {
        let text = "note: |\n  # not a comment\n  text\nkeep: 1\n";
        let mut d = doc(text);
        d.set(&["note"], ystr("short")).unwrap();
        assert_eq!(d.text(), "note: short\nkeep: 1\n");
    }

    #[test]
    fn flow_root_and_multi_document_files_are_refused() {
        assert!(matches!(
            YamlDocument::parse("{a: 1}", "x"),
            Err(Error::Unsupported { .. })
        ));
        assert!(matches!(
            YamlDocument::parse("a: 1\n---\nb: 2\n", "x"),
            Err(Error::Unsupported { .. })
        ));
        assert!(matches!(YamlDocument::parse("a: [1", "x"), Err(Error::Parse { .. })));
    }

    #[test]
    fn last_line_without_newline_gets_one_before_an_appended_key() {
        let mut d = doc("a: 1");
        d.set(&["b"], ystr("2x")).unwrap();
        assert_eq!(d.text(), "a: 1\nb: 2x\n");
        let mut d = doc("a:\n  b: 1");
        d.set(&["a", "b"], Yaml::Integer(2)).unwrap();
        assert_eq!(d.text(), "a:\n  b: 2");
    }

    #[test]
    fn mappings_inside_sequences_render_and_read_back() {
        let mut d = doc("");
        let scoped = ymap([
            ("path", ystr("~/work")),
            ("models", ystrs(["a/b", "c"])),
        ]);
        d.set(&["enabledModels"], Yaml::Array(vec![ystr("x"), scoped.clone()]))
            .unwrap();
        assert_eq!(
            d.text(),
            "enabledModels:\n  - x\n  - path: ~/work\n    models:\n      - a/b\n      - c\n"
        );
        assert_eq!(d.get(&["enabledModels"]).and_then(|v| v.as_vec()).map(Vec::len), Some(2));
    }

    #[test]
    fn quoted_keys_are_found() {
        let mut d = doc("modelOverrides:\n  \"qwen3:8b\":\n    name: Q\n  'it''s': 1\n");
        d.set(&["modelOverrides", "qwen3:8b", "name"], ystr("Qwen")).unwrap();
        d.set(&["modelOverrides", "it's"], Yaml::Integer(2)).unwrap();
        assert_eq!(
            d.text(),
            "modelOverrides:\n  \"qwen3:8b\":\n    name: Qwen\n  'it''s': 2\n"
        );
    }
}
