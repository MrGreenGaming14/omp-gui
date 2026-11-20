//! `fs/read_text_file` and `fs/write_text_file`, confined to the session cwd.

use crate::rpc::RpcError;
use crate::wire::{ReadTextFileRequest, WriteTextFileRequest, code};
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("{0} is outside the session directory")]
    Outside(PathBuf),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

impl From<FsError> for RpcError {
    fn from(e: FsError) -> Self {
        let code = match &e {
            FsError::Outside(_) => code::INVALID_PARAMS,
            FsError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound => code::RESOURCE_NOT_FOUND,
            FsError::Io { .. } => code::INTERNAL,
        };
        RpcError::remote(code, e.to_string())
    }
}

/// Resolves `path` (absolute, or relative to `root`) to a path inside `root`,
/// following symlinks of every existing ancestor so a link can't lead out.
/// `root` must be canonical.
pub fn resolve_inside(root: &Path, path: &str) -> Result<PathBuf, FsError> {
    let requested = root.join(path);
    let outside = || FsError::Outside(requested.clone());
    let mut lexical = PathBuf::new();
    for component in requested.components() {
        match component {
            Component::ParentDir => {
                if !lexical.pop() {
                    return Err(outside());
                }
            }
            Component::CurDir => {}
            other => lexical.push(other),
        }
    }
    // Canonicalize the longest existing ancestor and re-attach the rest.
    let mut existing = lexical.as_path();
    let mut rest = Vec::new();
    let canonical = loop {
        match existing.canonicalize() {
            Ok(canonical) => break canonical,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
                    return Err(outside());
                };
                rest.push(name);
                existing = parent;
            }
            Err(source) => return Err(FsError::Io { path: requested.clone(), source }),
        }
    };
    let mut resolved = canonical;
    resolved.extend(rest.iter().rev());
    if resolved.starts_with(root) { Ok(resolved) } else { Err(outside()) }
}

pub async fn read_text_file(root: &Path, request: ReadTextFileRequest) -> Result<String, FsError> {
    let path = resolve_inside(root, &request.path)?;
    let text = tokio::fs::read_to_string(&path).await.map_err(|source| FsError::Io { path, source })?;
    if request.line.is_none() && request.limit.is_none() {
        return Ok(text);
    }
    let skip = request.line.map_or(0, |line| line.saturating_sub(1)) as usize;
    let take = request.limit.map_or(usize::MAX, |limit| limit as usize);
    Ok(text.split_inclusive('\n').skip(skip).take(take).collect())
}

pub async fn write_text_file(root: &Path, request: WriteTextFileRequest) -> Result<(), FsError> {
    let path = resolve_inside(root, &request.path)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|source| FsError::Io { path: parent.to_owned(), source })?;
    }
    tokio::fs::write(&path, request.content).await.map_err(|source| FsError::Io { path, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        (dir, root)
    }

    #[test]
    fn dot_dot_cannot_climb_out() {
        let (_dir, root) = root();
        let escape = root.join("src/../../secret.txt");
        assert!(matches!(resolve_inside(&root, escape.to_str().unwrap()), Err(FsError::Outside(_))));
        assert!(matches!(resolve_inside(&root, "../secret.txt"), Err(FsError::Outside(_))));
        let inside = root.join("src/../notes.txt");
        assert_eq!(resolve_inside(&root, inside.to_str().unwrap()).unwrap(), root.join("notes.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_out_of_the_root_is_refused() {
        let (dir, root) = root();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();
        let through_link = root.join("link/new/file.txt");
        assert!(matches!(resolve_inside(&root, through_link.to_str().unwrap()), Err(FsError::Outside(_))));
    }

    #[test]
    fn missing_files_resolve_under_the_root() {
        let (_dir, root) = root();
        let path = root.join("a/b/c.txt");
        assert_eq!(resolve_inside(&root, path.to_str().unwrap()).unwrap(), path);
    }

    #[tokio::test]
    async fn line_and_limit_select_lines() {
        let (_dir, root) = root();
        std::fs::write(root.join("f.txt"), "one\ntwo\nthree\nfour").unwrap();
        let path = root.join("f.txt").to_str().unwrap().to_owned();
        let text = read_text_file(&root, ReadTextFileRequest { path, line: Some(2), limit: Some(2) }).await.unwrap();
        assert_eq!(text, "two\nthree\n");
    }
}
