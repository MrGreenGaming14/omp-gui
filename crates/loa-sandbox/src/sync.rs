//! Transfer workspaces through the stub when the provider does not mount a
//! host directory. Git administration files stay on the host: in particular,
//! a linked worktree's `.git` file points at a host-only path.

use crate::client::StubClient;
use crate::proto::{Command, ExecRequest, FileKind};
use crate::{Result, SandboxError};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_FILE: u64 = (crate::proto::MAX_FRAME as u64) - 1024;

fn invalid(message: impl Into<String>) -> SandboxError {
    SandboxError::Isolation(message.into())
}

fn relative(path: &Path) -> Result<String> {
    let path = path.to_str().ok_or_else(|| invalid(format!("non-UTF8 workspace path: {}", path.display())))?;
    if path.is_empty() || path.split('/').any(|component| component == "." || component == ".." || component.is_empty()) {
        return Err(invalid(format!("invalid workspace path: {path}")));
    }
    Ok(path.to_owned())
}

fn child_path(parent: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(invalid(format!("invalid stub directory entry: {name:?}")));
    }
    Ok(parent.join(name))
}

async fn run(client: &StubClient, program: &str, arguments: &[&str]) -> Result<Vec<u8>> {
    let mut argv = Vec::with_capacity(arguments.len() + 1);
    argv.push(program.to_owned());
    argv.extend(arguments.iter().map(|s| (*s).to_owned()));
    let output = client.exec(ExecRequest {
        command: Command::Argv(argv), cwd: None, env: Vec::new(), stdin: None, timeout_ms: None,
    }).await?;
    if !output.status.success() {
        return Err(invalid(format!("{program} failed: {}", String::from_utf8_lossy(&output.stderr))));
    }
    Ok(output.stdout)
}

fn check_size(path: &Path, size: u64) -> Result<()> {
    if size > MAX_FILE {
        return Err(invalid(format!("{} exceeds the stub's single-file protocol limit of {MAX_FILE} bytes", path.display())));
    }
    Ok(())
}

async fn send_entry(client: &StubClient, root: &Path, path: &Path) -> Result<()> {
    let relative_path = path.strip_prefix(root).map_err(|e| invalid(e.to_string()))?;
    let remote = relative(relative_path)?;
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        run(client, "mkdir", &["-p", "--", &remote]).await?;
        for entry in fs::read_dir(path)? {
            send_entry_boxed(client, root, &entry?.path()).await?;
        }
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            let mode = format!("{:o}", meta.permissions().mode() & 0o7777);
            run(client, "chmod", &[&mode, "--", &remote]).await?;
        }
    } else if meta.file_type().is_symlink() {
        let target = fs::read_link(path)?;
        let target = target.to_str().ok_or_else(|| invalid(format!("non-UTF8 symlink: {}", path.display())))?;
        run(client, "ln", &["-s", "--", target, &remote]).await?;
    } else if meta.is_file() {
        check_size(path, meta.len())?;
        #[cfg(unix)]
        let mode = { use std::os::unix::fs::PermissionsExt; Some(meta.permissions().mode() & 0o7777) };
        #[cfg(not(unix))]
        let mode = None;
        client.write_file(&remote, fs::read(path)?, mode).await?;
    } else {
        return Err(invalid(format!("unsupported workspace entry: {}", path.display())));
    }
    Ok(())
}

fn send_entry_boxed<'a>(client: &'a StubClient, root: &'a Path, path: &'a Path) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(send_entry(client, root, path))
}

/// Upload all files except host Git administration data to an empty stub root.
pub async fn copy_in(client: &StubClient, workspace: &Path) -> Result<()> {
    for entry in fs::read_dir(workspace)? {
        let entry = entry?;
        if entry.file_name() == ".git" { continue; }
        send_entry_boxed(client, workspace, &entry.path()).await?;
    }
    Ok(())
}

fn remove(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() { fs::remove_dir_all(path)?; } else { fs::remove_file(path)?; }
    Ok(())
}

async fn receive_dir(client: &StubClient, workspace: &Path, remote_dir: &Path) -> Result<()> {
    let remote = if remote_dir.as_os_str().is_empty() { ".".to_owned() } else { relative(remote_dir)? };
    let entries = client.list_dir(&remote).await?;
    let local_dir = workspace.join(remote_dir);
    fs::create_dir_all(&local_dir)?;
    for entry in fs::read_dir(&local_dir)? {
        let entry = entry?;
        if remote_dir.as_os_str().is_empty() && entry.file_name() == ".git" { continue; }
        let name = entry.file_name();
        if !entries.iter().any(|remote| name == remote.name.as_str()) {
            remove(&entry.path())?;
        }
    }
    for entry in entries {
        if remote_dir.as_os_str().is_empty() && entry.name == ".git" { continue; }
        let child = child_path(remote_dir, &entry.name)?;
        let remote = relative(&child)?;
        let meta = client.stat(&remote).await?.ok_or_else(|| invalid(format!("stub entry disappeared: {remote}")))?;
        let local = workspace.join(&child);
        if let Ok(existing) = fs::symlink_metadata(&local)
            && ((meta.kind == FileKind::Dir && !existing.is_dir())
                || (meta.kind != FileKind::Dir && existing.is_dir())
                || (meta.kind == FileKind::Symlink && !existing.file_type().is_symlink())
                || (meta.kind == FileKind::File && !existing.is_file()))
        {
            remove(&local)?;
        }
        match meta.kind {
            FileKind::Dir => receive_dir_boxed(client, workspace, &child).await?,
            FileKind::File => {
                check_size(&child, meta.size)?;
                let data = client.read_file(&remote, Some(MAX_FILE + 1)).await?;
                check_size(&child, data.len() as u64)?;
                fs::write(&local, data)?;
                #[cfg(unix)] {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&local, fs::Permissions::from_mode(meta.mode))?;
                }
            }
            FileKind::Symlink => {
                #[cfg(unix)] {
                    let bytes = run(client, "readlink", &["--", &remote]).await?;
                    let target = String::from_utf8(bytes).map_err(|e| invalid(e.to_string()))?;
                    let target = target.trim_end_matches('\n');
                    if fs::symlink_metadata(&local).is_ok() { remove(&local)?; }
                    std::os::unix::fs::symlink(target, &local)?;
                }
                #[cfg(not(unix))]
                return Err(invalid("symlink transfer requires Unix"));
            }
            FileKind::Other => return Err(invalid(format!("unsupported stub entry: {remote}"))),
        }
    }
    Ok(())
}

fn receive_dir_boxed<'a>(client: &'a StubClient, workspace: &'a Path, path: &'a Path) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(receive_dir(client, workspace, path))
}

/// Bring back additions, edits and removals. Only call this on an isolated
/// checkout; it deliberately does not modify the source workspace itself.
pub async fn copy_out(client: &StubClient, workspace: &Path) -> Result<()> {
    receive_dir(client, workspace, Path::new("")).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_paths_that_escape_or_collide_with_root() {
        assert!(child_path(Path::new("src"), "..").is_err());
        assert!(child_path(Path::new("src"), "a/b").is_err());
        assert!(relative(Path::new("../source")).is_err());
        assert_eq!(relative(Path::new("src/main.rs")).unwrap(), "src/main.rs");
    }
}
