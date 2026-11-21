//! Copies workspaces through the stub, as tar archives, when the provider
//! does not mount a host directory. Git administration files stay on the
//! host: in particular, a linked worktree's `.git` file points at a host-only path.

use crate::client::StubClient;
use crate::isolation::remove_entry;
use crate::proto::ExecRequest;
use crate::{Result, SandboxError};
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

/// Archive bytes per request, well under the frame limit.
const PART: usize = 16 << 20;
/// Where the archive collects in the stub root until it is unpacked.
const ARCHIVE: &str = ".loa-sandbox-in.tar";
const PACK: &[&str] = &["-cf", "-", "--exclude=./.git", "."];

/// Runs the host's tar in `dir` with `input` on its stdin.
async fn tar(dir: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    let mut child = tokio::process::Command::new("tar").args(args).current_dir(dir).kill_on_drop(true)
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

/// Uploads every file but the host's Git administration data to an empty stub root.
pub async fn copy_in(client: &StubClient, workspace: &Path) -> Result<()> {
    let archive = tar(workspace, PACK, &[]).await?;
    for (index, part) in archive.chunks(PART).enumerate() {
        let redirect = if index == 0 { ">" } else { ">>" };
        client.run(ExecRequest { stdin: Some(part.to_vec()), ..ExecRequest::shell(&format!("cat {redirect} {ARCHIVE}"), &[]) }).await?;
    }
    client.run(ExecRequest::shell(&format!("tar -xf {ARCHIVE} && rm -f {ARCHIVE}"), &[])).await.map(drop)
}

/// Replaces `workspace`, but for its Git data, with the stub root: brings
/// back additions, edits and removals.
pub async fn copy_out(client: &StubClient, workspace: &Path) -> Result<()> {
    let archive = client.run(ExecRequest { argv: ["tar"].iter().chain(PACK).map(|arg| (*arg).to_owned()).collect(), ..ExecRequest::default() }).await?;
    unpack(workspace, &archive).await
}

/// Applies an untrusted archive to `workspace`. It unpacks into a staging
/// directory first, so a failed unpack leaves the workspace untouched; the
/// archive's `.git` never reaches the workspace, and tar itself refuses
/// members that climb out with `..`.
async fn unpack(workspace: &Path, archive: &[u8]) -> Result<()> {
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
        let _ = unpack(&workspace, &escape).await;
        assert!(!root.path().join("new.txt").exists());
        assert!(workspace.join("new.txt").exists());
    }
}
