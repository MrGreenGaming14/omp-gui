//! [`NativeCodec`]: a harness's own session files, read into and written
//! from the DOM, and found on disk.

use loa_contracts::harness::Result;
use loa_core::{Dom, PatchSink};
use std::path::{Path, PathBuf};

/// One native session file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeSession {
    pub harness: &'static str,
    pub path: PathBuf,
    /// The harness's project folder name, when it groups sessions by one.
    pub project: Option<String>,
    pub title: Option<String>,
    /// Unix milliseconds of the last change.
    pub modified: i64,
    pub size: u64,
}

pub trait NativeCodec {
    /// Reads the native session at `path` into the empty `sink`, keeping original timestamps.
    fn import(path: &Path, sink: &dyn PatchSink) -> Result<()>;
    /// Writes `dom` as a native session that imports back to an equal DOM.
    fn export(dom: &Dom, out: &Path) -> Result<()>;
    /// The harness's sessions under the home directory `home`.
    fn discover(home: &Path) -> Vec<NativeSession>;
}

impl NativeSession {
    /// The session file at `path`, sized and dated from its metadata.
    pub fn at(harness: &'static str, path: PathBuf, project: Option<String>, title: Option<String>) -> Option<Self> {
        let meta = std::fs::metadata(&path).ok()?;
        let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
        let modified = modified.map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        Some(Self { harness, path, project, title, modified, size: meta.len() })
    }
}

/// Files exactly `depth` folders below `dir` whose name passes `keep`.
pub fn walk(dir: &Path, depth: usize, keep: &dyn Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        match entry.file_type() {
            Ok(t) if t.is_dir() && depth > 0 => out.extend(walk(&entry.path(), depth - 1, keep)),
            Ok(t) if t.is_file() && depth == 0 && keep(&entry.file_name().to_string_lossy()) => out.push(entry.path()),
            _ => {}
        }
    }
    out
}

/// The name of the folder holding `path`.
pub fn parent_name(path: &Path) -> Option<String> {
    path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned())
}
