//! File helpers shared by the config editors: optional reads, atomic writes and a
//! snapshot check that refuses to overwrite a file changed by someone else.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Reads `path`, returning `None` when it does not exist.
pub(crate) fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Error::io(path, err)),
    }
}

/// Writes `text` to `path` atomically: a temp file in the same directory is
/// written, synced and renamed over the target. A symlinked target is resolved so
/// the link survives and its destination is updated. Permissions of an existing
/// file are kept.
pub(crate) fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let target = resolve_symlink(path)?;
    let dir = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let mut tmp = tempfile::NamedTempFile::new_in(&dir).map_err(|e| Error::io(&dir, e))?;
    tmp.write_all(text.as_bytes())
        .map_err(|e| Error::io(tmp.path(), e))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| Error::io(tmp.path(), e))?;
    if let Ok(meta) = std::fs::metadata(&target) {
        std::fs::set_permissions(tmp.path(), meta.permissions())
            .map_err(|e| Error::io(tmp.path(), e))?;
    }
    tmp.persist(&target).map_err(|e| Error::io(&target, e.error))?;
    Ok(())
}

fn resolve_symlink(path: &Path) -> Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            std::fs::canonicalize(path).map_err(|e| Error::io(path, e))
        }
        _ => Ok(path.to_path_buf()),
    }
}

/// What a file held when it was loaded, so a save can detect a concurrent change.
#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    path: PathBuf,
    original: Option<String>,
}

impl Snapshot {
    /// Loads `path` and remembers its content.
    pub(crate) fn load(path: &Path) -> Result<(Self, Option<String>)> {
        let original = read_optional(path)?;
        Ok((
            Self {
                path: path.to_path_buf(),
                original: original.clone(),
            },
            original,
        ))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Writes `text` when the file still holds what was loaded, then remembers
    /// `text` as the new baseline.
    pub(crate) fn save(&mut self, text: &str) -> Result<()> {
        let current = read_optional(&self.path)?;
        if current != self.original {
            return Err(Error::ChangedOnDisk {
                path: self.path.clone(),
            });
        }
        write_atomic(&self.path, text)?;
        self.original = Some(text.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_refuses_when_file_changed_after_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yml");
        std::fs::write(&path, "a: 1\n").unwrap();
        let (mut snap, _) = Snapshot::load(&path).unwrap();
        std::fs::write(&path, "a: 2\n").unwrap();
        let err = snap.save("a: 3\n").unwrap_err();
        assert!(matches!(err, Error::ChangedOnDisk { .. }));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a: 2\n");
    }

    #[test]
    fn save_through_symlink_keeps_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.yml");
        let link = dir.path().join("config.yml");
        std::fs::write(&real, "a: 1\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (mut snap, _) = Snapshot::load(&link).unwrap();
        snap.save("a: 2\n").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "a: 2\n");
    }
}
