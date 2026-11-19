//! Import and export of omp's JSONL session files.
//!
//! Import maps the active path (leaf = last entry in the file, walked up by
//! `parentId`) onto the shared DOM shapes and keeps every entry's exact line,
//! so export reproduces the file:
//! - elements that stand for an entry carry the line in `x-omp-entry` and its
//!   position among the file's valid lines in `x-omp-line`
//! - `<ext harness="omp">` elements hold the line as their text and carry only
//!   `x-omp-line`
//! - entries off the active path become `<ext type="branch">` under the session
//!
//! Old files are migrated to version 3 on import the way omp does it, and
//! export writes the migrated form. Malformed lines are skipped like omp's
//! lenient loader does, so export doesn't reproduce them. DOM content added
//! after an import (live turns, compactions) is appended as new entries.

mod export;
mod import;
mod time;

use loa_adapter::AdapterError;
use loa_core::{Dom, PatchSink};
use std::path::Path;

/// Harness id written to `<session harness>` and `<ext harness>`.
pub const HARNESS: &str = "omp";

/// Attribute holding the exact JSON line of the entry an element stands for.
const X_ENTRY: &str = "x-omp-entry";
/// Attribute holding the position of that line among the file's non-blank,
/// valid lines, title slot and header included.
const X_LINE: &str = "x-omp-line";

/// Reads an omp JSONL session file into `sink` (which holds an empty DOM). `node` is the owner node key for `<session node>`.
pub fn import(path: &Path, sink: &dyn PatchSink, node: &str) -> Result<(), AdapterError> {
    let text = std::fs::read_to_string(path)?;
    import_str(&text, sink, node)
}

/// Same, from file contents already in memory.
pub fn import_str(text: &str, sink: &dyn PatchSink, node: &str) -> Result<(), AdapterError> {
    import::run(text, sink, node)
}

/// Writes `dom` as an omp JSONL session file at `out`.
pub fn export(dom: &Dom, out: &Path) -> Result<(), AdapterError> {
    let text = export_string(dom)?;
    std::fs::write(out, text)?;
    Ok(())
}

/// Same, returning the file contents.
pub fn export_string(dom: &Dom) -> Result<String, AdapterError> {
    export::run(dom)
}

/// Serializes one entry as a JSONL line (without the newline).
fn to_line(value: &impl serde::Serialize) -> Result<String, AdapterError> {
    serde_json::to_string(value).map_err(|e| AdapterError::Format(e.to_string()))
}
