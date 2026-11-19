//! Durable file replacement shared by state, staging and swap.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Replaces `path` with `bytes` atomically: writes and syncs a temporary file
/// in the same directory, renames it over `path`, then syncs the directory.
/// Creates the parent directory when missing.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    sync_dir(dir)
}

/// Makes renames inside `dir` durable.
#[cfg(unix)]
pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

/// Directories can't be opened for syncing here; renames are as durable as
/// the OS makes them.
#[cfg(not(unix))]
pub(crate) fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}
