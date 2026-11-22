//! Copies workspaces through the stub, as tar archives, when the provider
//! does not mount a host directory. Git administration files stay on the
//! host: in particular, a linked worktree's `.git` file points at a host-only path.
//!
//! Every member name of an archive from the guest is checked on its own
//! before anything is unpacked, so an archive the host can't take changes
//! nothing.

use loa_util::exec::NoWindow;
use crate::client::StubClient;
use crate::isolation::remove_entry;
use crate::proto::ExecRequest;
use crate::{Result, SandboxError};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

/// Archive bytes per request, well under the frame limit.
const PART: usize = 16 << 20;
/// Where the archive collects in the stub root until it is unpacked.
const ARCHIVE: &str = ".loa-sandbox-in.tar";
const PACK: &[&str] = &["-cf", "-", "--exclude=./.git", "."];

fn invalid(message: impl Into<String>) -> SandboxError {
    SandboxError::Isolation(message.into())
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

/// The names of an archive member's path, each checked; `./` and a trailing
/// `/` are dropped, and the root itself has none.
fn member_names(path: &[u8]) -> Result<Vec<String>> {
    let path = std::str::from_utf8(path).map_err(|_| invalid(format!("non-UTF8 workspace path: {}", String::from_utf8_lossy(path))))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_prefix("./").unwrap_or(path);
    if path.is_empty() || path == "." {
        return Ok(Vec::new());
    }
    path.split('/').map(|name| check_name(name).map(|()| name.to_owned())).collect()
}

/// Checks every member of an archive from the guest before any of it reaches
/// the host: each name, the member kinds this host can hold (no symlinks on
/// Windows), and names that differ only in case.
fn check_archive(archive: &[u8]) -> Result<()> {
    let mut dirs: HashMap<String, BTreeSet<String>> = HashMap::new();
    for entry in tar::Archive::new(archive).entries()? {
        let entry = entry?;
        let names = member_names(&entry.path_bytes())?;
        let wire = names.join("/");
        match entry.header().entry_type() {
            tar::EntryType::Regular | tar::EntryType::Continuous | tar::EntryType::Directory => {}
            // A hard link names another member of the archive.
            tar::EntryType::Link => drop(member_names(&entry.link_name_bytes().unwrap_or_default())?),
            tar::EntryType::Symlink if cfg!(windows) => {
                return Err(invalid(format!("guest symlink {wire} can't be copied to Windows; replace it with a regular file or remove it")));
            }
            tar::EntryType::Symlink => {}
            _ => return Err(invalid(format!("unsupported stub entry: {wire}"))),
        }
        for (depth, name) in names.iter().enumerate() {
            check_guest_name(name, depth == 0)?;
            dirs.entry(names[..depth].join("/")).or_default().insert(name.clone());
        }
    }
    for (dir, names) in &dirs {
        check_collisions(names.iter().map(String::as_str), if dir.is_empty() { "." } else { dir })?;
    }
    Ok(())
}

/// Links can't be copied to a guest from Windows: refuses them before
/// anything is sent, and returns the top level spellings of `.git` that
/// differ in case, since `.GIT` is the same directory there.
#[cfg(windows)]
fn check_sendable(dir: &Path, top: bool) -> Result<Vec<String>> {
    let mut git = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if top && name.eq_ignore_ascii_case(".git") {
            git.extend(name.to_str().filter(|name| *name != ".git").map(str::to_owned));
            continue;
        }
        let meta = std::fs::symlink_metadata(entry.path())?;
        if crate::isolation::is_link(&meta) {
            return Err(invalid(format!("{} is a link; links can't be copied to a guest from Windows", entry.path().display())));
        }
        if meta.is_dir() {
            check_sendable(&entry.path(), false)?;
        }
    }
    Ok(git)
}

/// Runs the host's tar in `dir` with `input` on its stdin.
async fn tar(dir: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    let mut child = tokio::process::Command::new("tar").no_window().args(args).current_dir(dir).kill_on_drop(true)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let feed = async move { stdin.write_all(input).await };
    let (fed, output) = tokio::join!(feed, child.wait_with_output());
    let output = output?;
    if !output.status.success() {
        return Err(SandboxError::Isolation(format!("tar failed: {}", String::from_utf8_lossy(&output.stderr).trim())));
    }
    fed?;
    Ok(output.stdout)
}

/// Uploads every file but the host's Git administration data to an empty
/// stub root. The guest needs a POSIX shell with `cat` and `tar`.
pub async fn copy_in(client: &StubClient, workspace: &Path) -> Result<()> {
    #[cfg(windows)]
    let excludes: Vec<String> = check_sendable(workspace, true)?.into_iter().map(|name| format!("--exclude=./{name}")).collect();
    #[cfg(not(windows))]
    let excludes: Vec<String> = Vec::new();
    let args: Vec<&str> = PACK[..PACK.len() - 1].iter().copied().chain(excludes.iter().map(String::as_str)).chain(PACK.last().copied()).collect();
    let archive = tar(workspace, &args, &[]).await?;
    for (index, part) in archive.chunks(PART).enumerate() {
        let redirect = if index == 0 { ">" } else { ">>" };
        client.run(ExecRequest { stdin: Some(part.to_vec()), ..ExecRequest::shell(&format!("cat {redirect} \"$1\""), &[ARCHIVE]) }).await?;
    }
    client.run(ExecRequest::shell(r#"tar -xf "$1" && rm -f "$1""#, &[ARCHIVE])).await.map(drop)
}

/// Replaces `workspace`, but for its Git data, with the stub root: brings
/// back additions, edits and removals.
pub async fn copy_out(client: &StubClient, workspace: &Path) -> Result<()> {
    let archive = client.run(ExecRequest { argv: ["tar"].iter().chain(PACK).map(|arg| (*arg).to_owned()).collect(), ..ExecRequest::default() }).await?;
    unpack(workspace, &archive).await
}

/// Applies an untrusted archive to `workspace`. Every member is checked
/// first ([`check_archive`]), then it unpacks into a staging directory, so a
/// refused archive or a failed unpack leaves the workspace untouched; the
/// archive's `.git` never reaches the workspace, and tar itself refuses
/// members that climb out with `..`.
async fn unpack(workspace: &Path, archive: &[u8]) -> Result<()> {
    check_archive(archive)?;
    let staging = tempfile::Builder::new().prefix(".loa-sandbox-out").tempdir_in(workspace)?;
    tar(staging.path(), &["-xf", "-", "--no-same-owner"], archive).await?;
    let mut kept = HashSet::new();
    for entry in std::fs::read_dir(staging.path())? {
        let name = entry?.file_name();
        if name != ".git" {
            let target = workspace.join(&name);
            remove_entry(&target)?;
            std::fs::rename(staging.path().join(&name), target)?;
            kept.insert(name);
        }
    }
    let staging_name = staging.path().file_name().map(OsString::from);
    for entry in std::fs::read_dir(workspace)? {
        let entry = entry?;
        let name = entry.file_name();
        if name != ".git" && !kept.contains(&name) && Some(&name) != staging_name.as_ref() {
            remove_entry(&entry.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unpacking needs GNU tar: `--transform` builds the escaping member.
    #[cfg(unix)]
    #[tokio::test]
    async fn unpack_keeps_git_and_refuses_escapes() {
        let root = tempfile::tempdir().unwrap();
        let (source, workspace) = (root.path().join("source"), root.path().join("ws"));
        std::fs::create_dir_all(source.join(".git")).unwrap();
        std::fs::write(source.join(".git/config"), "[core]\n\tfsmonitor = evil\n").unwrap();
        std::fs::write(source.join("new.txt"), "new").unwrap();
        std::fs::create_dir_all(workspace.join(".git")).unwrap();
        std::fs::write(workspace.join(".git/config"), "host").unwrap();
        std::fs::write(workspace.join("stale.txt"), "stale").unwrap();
        let archive = tar(&source, &["-cf", "-", "./.git", "./new.txt"], &[]).await.unwrap();
        unpack(&workspace, &archive).await.unwrap();
        assert_eq!(std::fs::read_to_string(workspace.join(".git/config")).unwrap(), "host");
        assert_eq!(std::fs::read_to_string(workspace.join("new.txt")).unwrap(), "new");
        assert!(!workspace.join("stale.txt").exists());
        assert_eq!(std::fs::read_dir(&workspace).unwrap().count(), 2);

        let escape = tar(&source, &["-cf", "-", "-P", "--transform=s,^,../,", "./new.txt"], &[]).await.unwrap();
        assert!(unpack(&workspace, &escape).await.is_err());
        assert!(!root.path().join("new.txt").exists());
        assert!(workspace.join("new.txt").exists());
    }

    #[test]
    fn rejects_names_that_escape_or_collide_with_root() {
        assert!(check_name("..").is_err());
        assert!(check_name("a/b").is_err());
        assert!(check_name("a\\b").is_err());
        assert!(check_name("src").is_ok());
        assert_eq!(member_names(b"./src/main.rs/").unwrap(), ["src", "main.rs"]);
        assert!(member_names(b"./").unwrap().is_empty());
        assert!(member_names(b"../source").is_err() && member_names(b"/etc/passwd").is_err() && member_names(b"a//b").is_err());
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

    /// A whole archive is refused before anything is unpacked when one member can't be held here.
    #[test]
    fn archives_are_checked_member_by_member() {
        let archive = |entries: &[(&str, tar::EntryType)]| {
            let mut builder = tar::Builder::new(Vec::new());
            for (path, kind) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(*kind);
                header.set_size(0);
                header.set_path(path).unwrap();
                header.set_cksum();
                builder.append(&header, std::io::empty()).unwrap();
            }
            builder.into_inner().unwrap()
        };
        assert!(check_archive(&archive(&[("./", tar::EntryType::Directory), ("./src/", tar::EntryType::Directory), ("./src/a.rs", tar::EntryType::Regular)])).is_ok());
        assert_eq!(check_archive(&archive(&[("./a.txt", tar::EntryType::Regular), ("./A.TXT", tar::EntryType::Regular)])).is_err(), cfg!(windows));
        assert_eq!(check_archive(&archive(&[("./CON", tar::EntryType::Regular)])).is_err(), cfg!(windows));
        assert!(check_archive(&archive(&[("./fifo", tar::EntryType::Fifo)])).is_err());
    }
}
