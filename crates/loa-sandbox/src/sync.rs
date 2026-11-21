//! Copies workspaces through the stub, as tar archives, when the provider
//! does not mount a host directory. Git administration files stay on the
//! host: in particular, a linked worktree's `.git` file points at a host-only path.

use crate::client::StubClient;
use crate::isolation::remove_entry;
use crate::proto::{Command, ExecRequest};
use crate::{Result, SandboxError};
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

/// Archive bytes per request, well under the frame limit.
const PART: usize = 16 << 20;
/// Where the archive collects in the stub root until it is unpacked.
const ARCHIVE: &str = ".loa-sandbox-in.tar";
const PACK: &[&str] = &["-cf", "-", "--exclude=./.git", "."];

fn failed(what: &str, stderr: &[u8]) -> SandboxError {
    SandboxError::Isolation(format!("{what} failed: {}", String::from_utf8_lossy(stderr).trim()))
}

/// Runs the host's tar in `dir` with `input` on its stdin.
async fn tar(dir: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    let mut child = tokio::process::Command::new("tar").args(args).current_dir(dir).kill_on_drop(true)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let feed = async move { stdin.write_all(input).await };
    let (fed, output) = tokio::join!(feed, child.wait_with_output());
    let output = output?;
    if !output.status.success() {
        return Err(failed("tar", &output.stderr));
    }
    fed?;
    Ok(output.stdout)
}

/// Runs `script` in the stub root with `stdin`.
async fn remote(client: &StubClient, script: String, stdin: Vec<u8>) -> Result<Vec<u8>> {
    let output = client.exec(ExecRequest { stdin: Some(stdin), ..ExecRequest::new(Command::Shell(script)) }).await?;
    if !output.status.success() {
        return Err(failed("the sandbox's tar", &output.stderr));
    }
    Ok(output.stdout)
}

/// Uploads every file but the host's Git administration data to an empty stub root.
pub async fn copy_in(client: &StubClient, workspace: &Path) -> Result<()> {
    let archive = tar(workspace, PACK, &[]).await?;
    for (index, part) in archive.chunks(PART).enumerate() {
        let redirect = if index == 0 { ">" } else { ">>" };
        remote(client, format!("cat {redirect} {ARCHIVE}"), part.to_vec()).await?;
    }
    remote(client, format!("tar -xf {ARCHIVE} && rm -f {ARCHIVE}"), Vec::new()).await.map(drop)
}

/// Replaces `workspace`, but for its Git data, with the stub root: brings
/// back additions, edits and removals.
pub async fn copy_out(client: &StubClient, workspace: &Path) -> Result<()> {
    let archive = remote(client, format!("tar {}", PACK.join(" ")), Vec::new()).await?;
    for entry in std::fs::read_dir(workspace)? {
        let entry = entry?;
        if entry.file_name() != ".git" {
            remove_entry(&entry.path())?;
        }
    }
    tar(workspace, &["-xf", "-"], &archive).await.map(drop)
}
