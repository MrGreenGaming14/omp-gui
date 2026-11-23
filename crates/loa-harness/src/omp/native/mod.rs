//! omp's JSONL session files. Import maps the active path (leaf = last
//! entry, walked up by `parentId`) onto the DOM and keeps every entry's exact
//! line, so export reproduces the file: elements standing for an entry carry
//! the line in `x-omp-entry` and its position in `x-omp-line`, `<ext
//! harness="omp">` holds other lines as text, entries off the active path
//! become `<ext type="branch">`. Old files are migrated to version 3 like omp
//! does; malformed lines are skipped. DOM content added after an import is
//! appended as new entries.

mod export;
mod history;
mod import;

pub use history::{ProjectSession, project_history, project_session};
pub(crate) use import::content_text;

use crate::kit::{NativeCodec, NativeSession, native};
use loa_contracts::harness::{AdapterError, Result};
use loa_core::{Dom, PatchSink};
use loa_util::time;
use std::path::Path;

use super::HARNESS;
/// The exact JSON line of the entry an element stands for.
const X_ENTRY: &str = "x-omp-entry";
/// The position of that line among the file's non-blank, valid lines.
const X_LINE: &str = "x-omp-line";
/// Root attribute naming the native file a session resumes from.
pub const X_SESSION_FILE: &str = "x-omp-session-file";

/// Reads an omp session file into the empty `sink`; `node` owns the session.
pub fn import(path: &Path, sink: &dyn PatchSink, node: &str) -> Result<()> {
    import_str(&std::fs::read_to_string(path)?, sink, node)
}

pub fn import_str(text: &str, sink: &dyn PatchSink, node: &str) -> Result<()> {
    import::run(text, sink, node)
}

pub fn export_string(dom: &Dom) -> Result<String> {
    export::run(dom)
}

/// The omp [`NativeCodec`]; imports are owned by the local node (`""`).
pub struct OmpNative;

impl NativeCodec for OmpNative {
    fn import(path: &Path, sink: &dyn PatchSink) -> Result<()> {
        import(path, sink, "")
    }

    fn export(dom: &Dom, out: &Path) -> Result<()> {
        Ok(std::fs::write(out, export_string(dom)?)?)
    }

    /// `~/.omp/agent/sessions/<project>/<name>.jsonl`, titled by their `title` line.
    fn discover(home: &Path) -> Vec<NativeSession> {
        let files = native::walk(&home.join(".omp/agent/sessions"), 1, &|name| name.ends_with(".jsonl"));
        files.into_iter().filter_map(|path| {
            let first = std::fs::File::open(&path).ok().and_then(|f| std::io::BufRead::lines(std::io::BufReader::new(f)).next()?.ok());
            let first = first.and_then(|line| serde_json::from_str::<serde_json::Value>(&line).ok()).filter(|line| line["type"] == "title");
            let title = first.and_then(|line| line["title"].as_str().map(str::to_owned));
            NativeSession::at(HARNESS, path.clone(), native::parent_name(&path), title)
        }).collect()
    }
}

fn to_line(value: &impl serde::Serialize) -> Result<String, AdapterError> {
    serde_json::to_string(value).map_err(|e| AdapterError::Format(e.to_string()))
}
