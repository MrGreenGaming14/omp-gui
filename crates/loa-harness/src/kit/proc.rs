//! Harness processes: piped spawn that survives `ETXTBSY`, a stderr tail,
//! and the error mapping every adapter shares.

use super::rpc::RpcError;
use loa_contracts::harness::AdapterError;
use loa_util::exec::{StderrTail, retry_busy};
use std::process::Stdio;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::oneshot;

/// Lines of stderr kept for error reports.
pub const STDERR_LINES: usize = 64;

/// A spawned child with its pipes taken.
pub struct Piped {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub stderr: StderrTail,
}

/// Spawns `cmd` with piped stdio, killed on drop. A freshly written
/// executable (a staged update, a test fake) can still be open for writing
/// in a child another thread forked; that clears once it execs, so a busy
/// executable is retried briefly.
pub fn spawn_piped(cmd: &mut Command) -> Result<Piped, RpcError> {
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = retry_busy(|| cmd.spawn()).map_err(|err| spawn_error(err, cmd))?;
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        return Err(RpcError::Protocol("child pipes were not created".to_owned()));
    };
    Ok(Piped { child, stdin, stdout, stderr: StderrTail::spawn(stderr, STDERR_LINES) })
}

/// A missing executable is [`RpcError::NotFound`]; anything else is i/o.
pub fn spawn_error(err: std::io::Error, cmd: &Command) -> RpcError {
    match err.kind() {
        std::io::ErrorKind::NotFound => RpcError::NotFound(cmd.as_std().get_program().to_string_lossy().into_owned()),
        _ => RpcError::Io(err),
    }
}

/// Waits for `child` to exit; kills it once `kill` fires (at once when `None`).
pub async fn reap(mut child: Child, kill: Option<oneshot::Receiver<()>>) -> Option<std::process::ExitStatus> {
    if let Some(kill) = kill {
        tokio::select! {
            status = child.wait() => return status.ok(),
            _ = kill => {}
        }
    }
    let _ = child.kill().await;
    child.wait().await.ok()
}

/// Bytes a process wrote, cut from the front to stay under `limit`, never
/// inside a UTF-8 sequence.
#[derive(Debug, Default)]
pub struct TailBuffer {
    bytes: Vec<u8>,
    limit: Option<usize>,
    truncated: bool,
}

impl TailBuffer {
    pub fn new(limit: Option<usize>) -> Self {
        Self { limit, ..Self::default() }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        let Some(over) = self.limit.and_then(|limit| self.bytes.len().checked_sub(limit)).filter(|over| *over > 0) else { return };
        let cut = (over..self.bytes.len()).find(|i| self.bytes[*i] & 0b1100_0000 != 0b1000_0000).unwrap_or(self.bytes.len());
        self.bytes.drain(..cut);
        self.truncated = true;
    }

    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
    }

    /// Whether bytes were cut.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

impl From<RpcError> for AdapterError {
    fn from(err: RpcError) -> Self {
        match err {
            RpcError::NotFound(program) => AdapterError::NotInstalled(program),
            RpcError::Io(err) => AdapterError::Io(err),
            RpcError::Closed => AdapterError::Closed,
            other => AdapterError::Protocol(other.to_string()),
        }
    }
}
