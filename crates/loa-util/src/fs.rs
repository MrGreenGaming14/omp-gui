//! Durable file writes, change-detecting snapshots, path confinement and well-known dirs.

use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

/// Replaces `path` with `bytes` via temp file, fsync, rename and dir sync. A symlinked
/// `path` keeps its link; `mode` (unix) or else the existing file's permissions apply.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let linked = fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink());
    let target = if linked { fs::canonicalize(path)? } else { path.to_path_buf() };
    let dir = target.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    #[cfg(unix)]
    let perms = mode.map(<fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode);
    #[cfg(not(unix))]
    let perms = mode.and(None::<fs::Permissions>);
    if let Some(perms) = perms.or_else(|| fs::metadata(&target).ok().map(|meta| meta.permissions())) {
        tmp.as_file().set_permissions(perms)?;
    }
    tmp.as_file().sync_all()?;
    tmp.persist(&target).map_err(|e| e.error)?;
    sync_dir(dir)
}

/// [`write_atomic`] of `v` as pretty JSON.
pub fn write_json_atomic<T: serde::Serialize>(path: &Path, v: &T) -> io::Result<()> { write_atomic(path, &serde_json::to_vec_pretty(v)?, None) }

/// The file's bytes, or `None` when it does not exist.
pub fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    fs::read(path).map(Some).or_else(|err| if err.kind() == io::ErrorKind::NotFound { Ok(None) } else { Err(err) })
}

/// Makes a rename in `dir` durable (no-op where directories can't be synced).
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// What a file held when loaded, so a save can refuse to clobber someone else's change.
#[derive(Clone, Debug)]
pub struct Snapshot {
    path: PathBuf,
    original: Option<Vec<u8>>,
}

impl Snapshot {
    pub fn load(path: &Path) -> io::Result<Self> { Ok(Self { path: path.to_path_buf(), original: read_optional(path)? }) }
    pub fn path(&self) -> &Path { &self.path }
    /// The bytes seen at load or last save; `None` when the file was missing.
    pub fn contents(&self) -> Option<&[u8]> { self.original.as_deref() }

    /// Writes `bytes` and returns true when the file still holds what was seen; false (no write) otherwise.
    pub fn save(&mut self, bytes: &[u8]) -> io::Result<bool> {
        if read_optional(&self.path)? != self.original {
            return Ok(false);
        }
        write_atomic(&self.path, bytes, None)?;
        self.original = Some(bytes.to_vec());
        Ok(true)
    }
}

/// `rel` under `root`, refusing `..`, absolute parts and symlinks that lead outside `root`.
pub fn confine(root: &Path, rel: &Path) -> io::Result<PathBuf> {
    let escape = || io::Error::new(io::ErrorKind::PermissionDenied, format!("{} is outside {}", rel.display(), root.display()));
    let root = fs::canonicalize(root)?;
    if rel.components().any(|part| !matches!(part, Component::Normal(_) | Component::CurDir)) {
        return Err(escape());
    }
    let joined = root.join(rel.components().filter(|part| *part != Component::CurDir).collect::<PathBuf>());
    let (mut existing, mut missing) = (joined.as_path(), Vec::new());
    let real = loop {
        match fs::canonicalize(existing) {
            Ok(real) => break real,
            Err(err) if err.kind() == io::ErrorKind::NotFound && fs::symlink_metadata(existing).is_err() => {
                missing.extend(existing.file_name());
                existing = existing.parent().ok_or_else(escape)?;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(escape()),
            Err(err) => return Err(err),
        }
    };
    if !real.starts_with(&root) {
        return Err(escape());
    }
    Ok(missing.into_iter().rev().fold(real, |path, name| path.join(name)))
}

/// The user's home directory (`HOME`, else `USERPROFILE`), when absolute.
pub fn home() -> Option<PathBuf> { env_dir("HOME").or_else(|| env_dir("USERPROFILE")) }

/// `$XDG_DATA_HOME/<app>`, else `~/.local/share/<app>`, else `%APPDATA%\<app>`. Not created.
pub fn data_dir(app: &str) -> Option<PathBuf> {
    let local = || env_dir("HOME").map(|home| home.join(".local").join("share"));
    Some(env_dir("XDG_DATA_HOME").or_else(local).or_else(|| env_dir("APPDATA"))?.join(app))
}

fn env_dir(key: &str) -> Option<PathBuf> { std::env::var_os(key).map(PathBuf::from).filter(|path| path.is_absolute()) }

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn writes_snapshots_and_confinement() {
        let dir = tempfile::tempdir().unwrap();
        let (real, link) = (dir.path().join("real"), dir.path().join("link"));
        write_atomic(&real, b"a", Some(0o600)).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut snap = Snapshot::load(&link).unwrap();
        assert!(snap.save(b"b").unwrap());
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        fs::write(&real, b"other").unwrap();
        assert!(!snap.save(b"c").unwrap());
        assert_eq!((read_optional(&real).unwrap(), read_optional(&dir.path().join("none")).unwrap()), (Some(b"other".to_vec()), None));
        std::os::unix::fs::symlink("/", dir.path().join("out")).unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        for (rel, ok) in [("a/b", Some("a/b")), ("./real", Some("real")), ("../x", None), ("/etc", None), ("out/etc", None), ("link", Some("real"))] {
            assert_eq!(confine(dir.path(), Path::new(rel)).ok(), ok.map(|p| root.join(p)), "{rel}");
        }
    }
}
