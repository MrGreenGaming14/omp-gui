//! Transfer workspaces through the stub when the provider does not mount a
//! host directory. Git administration files stay on the host: in particular,
//! a linked worktree's `.git` file points at a host-only path.
//!
//! Paths on the wire are `/`-joined names, each checked on its own; no OS
//! path ever becomes one. Bringing files back first scans and validates the
//! guest's whole tree, so a listing the host can't take changes nothing.

use crate::client::StubClient;
use crate::proto::{Command, ExecRequest, FileKind, Metadata};
use crate::{Result, SandboxError};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;

const MAX_FILE: u64 = (crate::proto::MAX_FRAME as u64) - 1024;

fn invalid(message: impl Into<String>) -> SandboxError {
    SandboxError::Isolation(message.into())
}

/// One name of a workspace path, as it goes on the wire.
fn wire_name(name: &OsStr) -> Result<&str> {
    let name = name.to_str().ok_or_else(|| invalid(format!("non-UTF8 workspace path: {}", name.to_string_lossy())))?;
    check_name(name)?;
    Ok(name)
}

/// `parent` is `.` for the workspace root.
fn join_wire(parent: &str, name: &str) -> String {
    if parent == "." { name.to_owned() } else { format!("{parent}/{name}") }
}

/// The names no platform takes as a single path component.
fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(invalid(format!("invalid directory entry: {name:?}")));
    }
    Ok(())
}

/// Why Windows can't hold a file called `name` as the guest named it, if so:
/// names with drive or stream separators, characters NTFS rejects, a trailing
/// dot or space (silently dropped), the DOS device names, and `GIT~1`, the
/// short name of `.git`.
fn windows_name_problem(name: &str) -> Option<&'static str> {
    if name.contains(':') {
        return Some("contains ':' (a drive or stream)");
    }
    if name.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*')) {
        return Some("contains a character Windows does not allow");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Some("ends with a dot or a space");
    }
    // `CON.txt` and `CON .txt` name the device too.
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ').to_uppercase();
    const DEVICES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix).is_some_and(|rest| matches!(rest.chars().collect::<Vec<_>>()[..], [c] if matches!(c, '1'..='9' | '¹' | '²' | '³')))
    };
    if DEVICES.contains(&stem.as_str()) || numbered("COM") || numbered("LPT") {
        return Some("is a reserved device name");
    }
    if name.eq_ignore_ascii_case("git~1") {
        return Some("is the short name of .git");
    }
    None
}

/// The checks on one entry of a guest directory that apply on this host.
fn check_guest_name(name: &str, top: bool) -> Result<()> {
    check_name(name)?;
    if !cfg!(windows) {
        return Ok(());
    }
    if let Some(problem) = windows_name_problem(name) {
        return Err(invalid(format!("guest entry {name:?} can't be copied to Windows: it {problem}")));
    }
    // The checkout's own `.git` is never replaced, whatever case the guest uses.
    if name.eq_ignore_ascii_case(".git") && !(top && name == ".git") {
        return Err(invalid(format!("guest entry {name:?} can't be copied to Windows: it would replace .git")));
    }
    Ok(())
}

/// Windows file names ignore case; two entries differing only in case would
/// overwrite each other.
fn check_collisions<'a>(names: impl Iterator<Item = &'a str>, dir: &str) -> Result<()> {
    if !cfg!(windows) {
        return Ok(());
    }
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name.to_lowercase()) {
            return Err(invalid(format!("guest entries of {dir} differ only in case ({name:?}); Windows can't hold both")));
        }
    }
    Ok(())
}

/// Runs a POSIX tool on the guest; Windows hosts never ask for one.
#[cfg_attr(windows, allow(dead_code))]
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

fn check_size(path: &str, size: u64) -> Result<()> {
    if size > MAX_FILE {
        return Err(invalid(format!("{path} exceeds the stub's single-file protocol limit of {MAX_FILE} bytes")));
    }
    Ok(())
}

async fn send_entry(client: &StubClient, path: &Path, wire: &str) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if crate::isolation::is_link(&meta) {
        #[cfg(unix)]
        {
            let target = fs::read_link(path)?;
            let target = target.to_str().ok_or_else(|| invalid(format!("non-UTF8 symlink: {}", path.display())))?;
            run(client, "ln", &["-s", "--", target, wire]).await?;
        }
        #[cfg(not(unix))]
        return Err(invalid(format!("{} is a link; links can't be copied to a guest from Windows", path.display())));
    } else if meta.is_dir() {
        let mut entries = Vec::new();
        for entry in fs::read_dir(path)? {
            entries.push(entry?);
        }
        if entries.is_empty() {
            // No file write makes an empty directory, and the guest may not
            // have `mkdir` at all: the guests Windows can reach are Linux
            // peers, so this is refused rather than guessed.
            #[cfg(not(unix))]
            return Err(invalid(format!("{} is an empty directory; empty directories can't be copied to a guest from Windows", path.display())));
            #[cfg(unix)]
            run(client, "mkdir", &["-p", "--", wire]).await?;
        }
        for entry in entries {
            let child = join_wire(wire, wire_name(&entry.file_name())?);
            send_entry_boxed(client, &entry.path(), &child).await?;
        }
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            let mode = format!("{:o}", meta.permissions().mode() & 0o7777);
            run(client, "chmod", &[&mode, "--", wire]).await?;
        }
    } else if meta.is_file() {
        check_size(wire, meta.len())?;
        #[cfg(unix)]
        let mode = { use std::os::unix::fs::PermissionsExt; Some(meta.permissions().mode() & 0o7777) };
        #[cfg(not(unix))]
        let mode = None;
        // The stub creates the file's parent directories.
        client.write_file(wire, fs::read(path)?, mode).await?;
    } else {
        return Err(invalid(format!("unsupported workspace entry: {}", path.display())));
    }
    Ok(())
}

fn send_entry_boxed<'a>(client: &'a StubClient, path: &'a Path, wire: &'a str) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(send_entry(client, path, wire))
}

/// Upload all files except host Git administration data to an empty stub root.
pub async fn copy_in(client: &StubClient, workspace: &Path) -> Result<()> {
    for entry in fs::read_dir(workspace)? {
        let entry = entry?;
        let name = entry.file_name();
        // On Windows `.GIT` is the same directory as `.git`.
        let git = if cfg!(windows) { name.eq_ignore_ascii_case(".git") } else { name == ".git" };
        if git { continue; }
        send_entry_boxed(client, &entry.path(), wire_name(&name)?).await?;
    }
    Ok(())
}

fn remove(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() { fs::remove_dir_all(path)?; } else { fs::remove_file(path)?; }
    Ok(())
}

/// One entry of the guest's tree, checked.
struct Node {
    name: String,
    meta: Metadata,
    wire: String,
    children: Vec<Node>,
}

/// Reads and validates everything under `wire_dir` without touching the host.
async fn scan(client: &StubClient, wire_dir: &str, top: bool) -> Result<Vec<Node>> {
    let entries = client.list_dir(wire_dir).await?;
    for entry in &entries {
        check_guest_name(&entry.name, top)?;
    }
    check_collisions(entries.iter().map(|entry| entry.name.as_str()), wire_dir)?;
    let mut nodes = Vec::with_capacity(entries.len());
    for entry in entries {
        if top && entry.name == ".git" { continue; }
        let wire = join_wire(wire_dir, &entry.name);
        let meta = client.stat(&wire).await?.ok_or_else(|| invalid(format!("stub entry disappeared: {wire}")))?;
        let children = match meta.kind {
            FileKind::Dir => scan_boxed(client, &wire).await?,
            FileKind::File => {
                check_size(&wire, meta.size)?;
                Vec::new()
            }
            FileKind::Symlink if cfg!(windows) => {
                return Err(invalid(format!("guest symlink {wire} can't be copied to Windows; replace it with a regular file or remove it")));
            }
            FileKind::Symlink => Vec::new(),
            FileKind::Other => return Err(invalid(format!("unsupported stub entry: {wire}"))),
        };
        nodes.push(Node { name: entry.name, meta, wire, children });
    }
    Ok(nodes)
}

fn scan_boxed<'a>(client: &'a StubClient, wire_dir: &'a str) -> std::pin::Pin<Box<dyn Future<Output = Result<Vec<Node>>> + Send + 'a>> {
    Box::pin(scan(client, wire_dir, false))
}

/// Makes `local_dir` match the scanned `nodes`.
async fn apply(client: &StubClient, local_dir: &Path, top: bool, nodes: &[Node]) -> Result<()> {
    fs::create_dir_all(local_dir)?;
    for entry in fs::read_dir(local_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if top && name == ".git" { continue; }
        // Exact names: a guest rename that only changes case is replaced,
        // not kept under the old spelling.
        if !nodes.iter().any(|node| name == node.name.as_str()) {
            remove(&entry.path())?;
        }
    }
    for node in nodes {
        let local = local_dir.join(&node.name);
        if let Ok(existing) = fs::symlink_metadata(&local)
            && ((node.meta.kind == FileKind::Dir && !existing.is_dir())
                || (node.meta.kind != FileKind::Dir && existing.is_dir())
                || (node.meta.kind == FileKind::Symlink && !existing.file_type().is_symlink())
                || (node.meta.kind == FileKind::File && !existing.is_file()))
        {
            remove(&local)?;
        }
        match node.meta.kind {
            FileKind::Dir => apply_boxed(client, &local, &node.children).await?,
            FileKind::File => {
                let data = client.read_file(&node.wire, Some(MAX_FILE + 1)).await?;
                check_size(&node.wire, data.len() as u64)?;
                fs::write(&local, data)?;
                #[cfg(unix)] {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&local, fs::Permissions::from_mode(node.meta.mode))?;
                }
            }
            FileKind::Symlink => {
                #[cfg(unix)] {
                    let bytes = run(client, "readlink", &["--", &node.wire]).await?;
                    let target = String::from_utf8(bytes).map_err(|e| invalid(e.to_string()))?;
                    let target = target.trim_end_matches('\n');
                    if fs::symlink_metadata(&local).is_ok() { remove(&local)?; }
                    std::os::unix::fs::symlink(target, &local)?;
                }
                // `scan` refuses guest symlinks on Windows.
                #[cfg(not(unix))]
                return Err(invalid(format!("guest symlink {} can't be copied to Windows", node.wire)));
            }
            FileKind::Other => return Err(invalid(format!("unsupported stub entry: {}", node.wire))),
        }
    }
    Ok(())
}

fn apply_boxed<'a>(client: &'a StubClient, local_dir: &'a Path, nodes: &'a [Node]) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(apply(client, local_dir, false, nodes))
}

/// Bring back additions, edits and removals. Only call this on an isolated
/// checkout; it deliberately does not modify the source workspace itself.
/// The guest's whole tree is checked first; a tree that fails a check
/// leaves the checkout as it was.
pub async fn copy_out(client: &StubClient, workspace: &Path) -> Result<()> {
    let nodes = scan(client, ".", true).await?;
    apply(client, workspace, true, &nodes).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_names_that_escape_or_collide_with_root() {
        assert!(check_name("..").is_err());
        assert!(check_name("a/b").is_err());
        assert!(check_name("a\\b").is_err());
        assert!(wire_name(OsStr::new("src")).is_ok());
        assert_eq!(join_wire(".", "src"), "src");
        assert_eq!(join_wire("src", "main.rs"), "src/main.rs");
    }

    #[test]
    fn windows_cannot_hold_these_names() {
        for name in [
            "a:b", "c:", "file.txt:stream", "trailing.", "trailing ", "CON", "con", "Prn.txt", "aux", "NUL.tar.gz", "COM1", "lpt9.log",
            "CON .txt", "COM¹", "CONIN$", "a<b", "a*b", "bell\u{7}", "GIT~1",
        ] {
            assert!(windows_name_problem(name).is_some(), "{name:?}");
        }
        for name in ["main.rs", ".gitignore", "console", "COM0", "COM10", "LPT", "a b", ".git", "ünï.txt"] {
            assert!(windows_name_problem(name).is_none(), "{name:?}");
        }
    }

    #[test]
    fn colliding_and_git_names_are_checked_on_windows_only() {
        let collisions = check_collisions(["a.txt", "A.TXT"].into_iter(), "src");
        assert_eq!(collisions.is_err(), cfg!(windows));
        assert!(check_collisions(["a.txt", "b.txt"].into_iter(), "src").is_ok());
        assert!(check_guest_name(".git", true).is_ok());
        assert_eq!(check_guest_name(".GIT", true).is_err(), cfg!(windows));
        assert_eq!(check_guest_name(".git", false).is_err(), cfg!(windows));
        assert_eq!(check_guest_name("CON", false).is_err(), cfg!(windows));
    }
}
