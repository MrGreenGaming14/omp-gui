//! Import and export of Claude Code's own transcripts
//! (`~/.claude/projects/<slug>/<session>.jsonl` plus the `<session>/` side
//! directory).
//!
//! Import maps every line to DOM elements and keeps whatever the DOM doesn't
//! hold in `x-claude*` attributes, so [`export`] rewrites the file line for
//! line:
//! - `x-claude`: the line's JSON object without `message.content` (the
//!   envelope), on the first element made from the line
//! - `x-claude-line`: index of the source line, on every element made from it
//! - `x-claude-part`: index of the content block, when `message.content` is
//!   an array
//! - `x-claude-block`: the block's JSON object without the fields the element
//!   holds (text, tool name, input, result content)
//! - `x-claude-content` and `x-claude-split`: how a result or compaction text
//!   was encoded (`string`, `texts` with the byte length of each text block,
//!   `json` or `absent`)
//! - `x-claude-file`: on a subagent `<job>`, the subagent transcript relative
//!   to the side directory
//! - `x-path`: on a side file `<ext type="file">`, its path relative to the
//!   side directory
//!
//! Lines the mapping doesn't cover are `<ext harness="claude-code">` holding
//! the line verbatim. Elements without these attributes (sessions the app
//! created or continued) export as fresh Claude Code lines.

mod content;
mod export;
mod import;
use loa_util::time;

use loa_adapter::Result;
use loa_core::{Dom, PatchSink};
use std::path::{Path, PathBuf};

/// The line's JSON object without `message.content`, as JSON text.
const X_ENVELOPE: &str = "x-claude";
/// Index (Int) of the transcript line an element came from.
const X_LINE: &str = "x-claude-line";
/// Index (Int) of the content block within its line.
const X_PART: &str = "x-claude-part";
/// The content block without the fields the element holds, as JSON text.
const X_BLOCK: &str = "x-claude-block";
/// Encoding of a result or compaction text, see [`content`].
const X_CONTENT: &str = "x-claude-content";
/// Byte lengths (Int list) of the text blocks joined into a `texts` text.
const X_SPLIT: &str = "x-claude-split";
/// Subagent transcript of a `<job>`, relative to the side directory.
const X_FILE: &str = "x-claude-file";
/// Side file path relative to the side directory.
const X_PATH: &str = "x-path";

/// `<ext type>` of a content block the mapping doesn't cover.
const EXT_BLOCK: &str = "block";
/// `<ext type>` of a side file.
const EXT_FILE: &str = "file";

/// Imports one Claude Code transcript file into `sink` (empty DOM). Side files
/// in the sibling `<stem>/` directory (subagents/*.jsonl, *.meta.json,
/// *.prefix.json, tool-results/*) are kept too.
///
/// The root gets `id` = the transcript's session id and `node` = "": the app
/// re-sets both after import. A subagent `<job>` carries `x-claude-file`, the
/// subagent transcript relative to the side directory, for the app to import
/// as the linked child session and set `session`.
pub fn import(path: &Path, sink: &dyn PatchSink) -> Result<()> {
    import::run(path, sink)
}

/// Writes `dom` as a transcript at `out` (a .jsonl path) plus its side files
/// under `out` without extension.
pub fn export(dom: &Dom, out: &Path) -> Result<()> {
    export::run(dom, out)
}

/// `<home>/.claude/projects`.
pub fn projects_dir(home: &Path) -> PathBuf {
    home.join(".claude").join("projects")
}

/// Claude Code's directory name for a cwd: every char that is not ASCII
/// alphanumeric becomes '-' ("/home/a/.x_y" -> "-home-a--x-y"). Claude Code
/// replaces UTF-16 code units, so a char outside the BMP becomes two.
pub fn project_slug(cwd: &Path) -> String {
    let mut slug = String::new();
    for c in cwd.to_string_lossy().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else {
            slug.extend(std::iter::repeat_n('-', c.len_utf16()));
        }
    }
    slug
}

/// `projects_dir(home)/project_slug(cwd)/<session_id>.jsonl`.
pub fn transcript_path(home: &Path, cwd: &Path, session_id: &str) -> PathBuf {
    projects_dir(home).join(project_slug(cwd)).join(format!("{session_id}.jsonl"))
}

/// The side directory of a transcript: its path without extension. `None`
/// when the path has no extension, since the directory would be the file.
fn side_dir(transcript: &Path) -> Option<PathBuf> {
    transcript.extension().map(|_| transcript.with_extension(""))
}

/// Removes `key` from `map` keeping the order of the other keys, which
/// `Map::remove` doesn't when serde_json's `preserve_order` is on. Imports
/// of the same line must give the same envelope whatever the feature set.
fn take_field(map: &mut serde_json::Map<String, serde_json::Value>, key: &str) -> Option<serde_json::Value> {
    let mut taken = None;
    map.retain(|name, value| {
        if name == key {
            taken = Some(value.take());
            false
        } else {
            true
        }
    });
    taken
}
