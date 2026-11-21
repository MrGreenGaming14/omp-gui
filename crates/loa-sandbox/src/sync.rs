//! Copies workspaces through the stub when the provider does not mount a
//! host directory. Git administration files stay on the host: in particular,
//! a linked worktree's `.git` file points at a host-only path.

use crate::client::StubClient;
use crate::isolation::remove_entry;
use crate::proto::{Command, ExecRequest, FileKind, MAX_FRAME};
use crate::{Result, SandboxError};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;

const MAX_FILE: u64 = MAX_FRAME as u64 - 1024;

type Step<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

fn invalid(message: impl Into<String>) -> SandboxError {
    SandboxError::Isolation(message.into())
}

/// `path` as a stub path: relative, UTF-8, without `.` or `..` components.
fn relative(path: &Path) -> Result<String> {
    let text = path.to_str().ok_or_else(|| invalid(format!("non-UTF8 workspace path: {}", path.display())))?;
    if text.is_empty() || text.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
        return Err(invalid(format!("invalid workspace path: {text}")));
    }
    Ok(text.to_owned())
}

fn child_path(parent: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(invalid(format!("invalid stub directory entry: {name:?}")));
    }
    Ok(parent.join(name))
}

async fn run(client: &StubClient, argv: &[&str]) -> Result<Vec<u8>> {
    let output = client.exec(ExecRequest::new(Command::Argv(argv.iter().map(|s| (*s).to_owned()).collect()))).await?;
    if !output.status.success() {
        return Err(invalid(format!("{} failed: {}", argv[0], String::from_utf8_lossy(&output.stderr))));
    }
    Ok(output.stdout)
}

fn check_size(path: &Path, size: u64) -> Result<()> {
    if size > MAX_FILE {
        return Err(invalid(format!("{} exceeds the stub's single-file protocol limit of {MAX_FILE} bytes", path.display())));
    }
    Ok(())
}

fn send<'a>(client: &'a StubClient, root: &'a Path, path: PathBuf) -> Step<'a> {
    Box::pin(async move {
        let remote = relative(path.strip_prefix(root).map_err(|e| invalid(e.to_string()))?)?;
        let meta = fs::symlink_metadata(&path)?;
        let mode = meta.permissions().mode() & 0o7777;
        if meta.is_dir() {
            run(client, &["mkdir", "-p", "--", &remote]).await?;
            for entry in fs::read_dir(&path)? {
                send(client, root, entry?.path()).await?;
            }
            run(client, &["chmod", &format!("{mode:o}"), "--", &remote]).await?;
        } else if meta.file_type().is_symlink() {
            let target = fs::read_link(&path)?;
            let target = target.to_str().ok_or_else(|| invalid(format!("non-UTF8 symlink: {}", path.display())))?;
            run(client, &["ln", "-s", "--", target, &remote]).await?;
        } else if meta.is_file() {
            check_size(&path, meta.len())?;
            client.write_file(&remote, fs::read(&path)?, Some(mode)).await?;
        } else {
            return Err(invalid(format!("unsupported workspace entry: {}", path.display())));
        }
        Ok(())
    })
}

/// Uploads every file but the host's Git administration data to an empty stub root.
pub async fn copy_in(client: &StubClient, workspace: &Path) -> Result<()> {
    for entry in fs::read_dir(workspace)? {
        let entry = entry?;
        if entry.file_name() != ".git" {
            send(client, workspace, entry.path()).await?;
        }
    }
    Ok(())
}

fn receive<'a>(client: &'a StubClient, workspace: &'a Path, dir: PathBuf) -> Step<'a> {
    Box::pin(async move {
        let top = dir.as_os_str().is_empty();
        let entries = client.list_dir(&if top { ".".to_owned() } else { relative(&dir)? }).await?;
        let entries: Vec<_> = entries.into_iter().filter(|e| !(top && e.name == ".git")).collect();
        let local_dir = workspace.join(&dir);
        fs::create_dir_all(&local_dir)?;
        for entry in fs::read_dir(&local_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if !(top && name == ".git") && !entries.iter().any(|remote| name == remote.name.as_str()) {
                remove_entry(&entry.path())?;
            }
        }
        for entry in entries {
            let child = child_path(&dir, &entry.name)?;
            let remote = relative(&child)?;
            let meta = client.stat(&remote).await?.ok_or_else(|| invalid(format!("stub entry disappeared: {remote}")))?;
            let local = workspace.join(&child);
            let same_kind = fs::symlink_metadata(&local).is_ok_and(|have| match meta.kind {
                FileKind::Dir => have.is_dir(),
                FileKind::File => have.is_file(),
                _ => false,
            });
            if !same_kind {
                remove_entry(&local)?;
            }
            match meta.kind {
                FileKind::Dir => receive(client, workspace, child).await?,
                FileKind::File => {
                    check_size(&child, meta.size)?;
                    let data = client.read_file(&remote, Some(MAX_FILE + 1)).await?;
                    check_size(&child, data.len() as u64)?;
                    fs::write(&local, data)?;
                    fs::set_permissions(&local, fs::Permissions::from_mode(meta.mode))?;
                }
                FileKind::Symlink => {
                    let target = String::from_utf8(run(client, &["readlink", "--", &remote]).await?).map_err(|e| invalid(e.to_string()))?;
                    std::os::unix::fs::symlink(target.trim_end_matches('\n'), &local)?;
                }
                FileKind::Other => return Err(invalid(format!("unsupported stub entry: {remote}"))),
            }
        }
        Ok(())
    })
}

/// Brings back additions, edits and removals. Only call this on an isolated
/// checkout; it deliberately does not modify the source workspace itself.
pub async fn copy_out(client: &StubClient, workspace: &Path) -> Result<()> {
    receive(client, workspace, PathBuf::new()).await
}
