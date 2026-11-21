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
