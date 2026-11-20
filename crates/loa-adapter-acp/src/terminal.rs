//! `terminal/*` client methods: commands the agent runs through the client,
//! with their working directory inside the session cwd.

use crate::fs::resolve_inside;
use crate::rpc::RpcError;
use crate::wire::{CreateTerminalRequest, code};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::{oneshot, watch};

/// The terminals of one session.
pub struct Terminals {
    /// Canonical session cwd.
    root: PathBuf,
    map: Mutex<HashMap<String, Arc<Terminal>>>,
    next: AtomicU64,
}

struct Terminal {
    output: Arc<Mutex<Output>>,
    exit: watch::Receiver<Option<Value>>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
}

/// Combined stdout and stderr, cut from the front to stay under the limit.
struct Output {
    bytes: Vec<u8>,
    limit: Option<usize>,
    truncated: bool,
}

impl Output {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        let Some(limit) = self.limit else { return };
        if self.bytes.len() <= limit {
            return;
        }
        let mut cut = self.bytes.len() - limit;
        // Never start the kept output inside a UTF-8 sequence.
        while cut < self.bytes.len() && (self.bytes[cut] & 0b1100_0000) == 0b1000_0000 {
            cut += 1;
        }
        self.bytes.drain(..cut);
        self.truncated = true;
    }
}

impl Terminals {
    /// `root` is the canonical session cwd.
    pub fn new(root: PathBuf) -> Self {
        Self { root, map: Mutex::new(HashMap::new()), next: AtomicU64::new(1) }
    }

    pub fn create(&self, request: CreateTerminalRequest) -> Result<Value, RpcError> {
        let cwd = match &request.cwd {
            Some(cwd) => resolve_inside(&self.root, cwd)?,
            None => self.root.clone(),
        };
        let mut command = Command::new(loa_exec::resolve(std::path::Path::new(&request.command)));
        command
            .args(request.args.unwrap_or_default())
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for var in request.env.unwrap_or_default() {
            command.env(var.name, var.value);
        }
        let mut child = command
            .spawn()
            .map_err(|e| RpcError::remote(code::INTERNAL, format!("can't run `{}`: {e}", request.command)))?;
        let limit = request.output_byte_limit.map(|l| usize::try_from(l).unwrap_or(usize::MAX));
        let output = Arc::new(Mutex::new(Output { bytes: Vec::new(), limit, truncated: false }));
        let readers = [
            child.stdout.take().map(|out| tokio::spawn(pump(out, output.clone()))),
            child.stderr.take().map(|err| tokio::spawn(pump(err, output.clone()))),
        ];
        let (exit_tx, exit_rx) = watch::channel(None);
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status,
                _ = kill_rx => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            // Publish the exit only once all output is in.
            for reader in readers.into_iter().flatten() {
                let _ = reader.await;
            }
            exit_tx.send_replace(Some(status.map_or_else(|_| json!({ "exitCode": null, "signal": null }), exit_json)));
        });
        let id = format!("term-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let terminal = Terminal { output, exit: exit_rx, kill: Mutex::new(Some(kill_tx)) };
        lock(&self.map).insert(id.clone(), Arc::new(terminal));
        Ok(json!({ "terminalId": id }))
    }

    pub fn output(&self, id: &str) -> Result<Value, RpcError> {
        let terminal = self.get(id)?;
        let exit = terminal.exit.borrow().clone();
        let output = lock(&terminal.output);
        Ok(json!({
            "output": String::from_utf8_lossy(&output.bytes),
            "truncated": output.truncated,
            "exitStatus": exit,
        }))
    }

    pub async fn wait_for_exit(&self, id: &str) -> Result<Value, RpcError> {
        let mut exit = self.get(id)?.exit.clone();
        let status = exit.wait_for(Option::is_some).await.map_err(|_| RpcError::Closed)?.clone();
        Ok(status.unwrap_or(Value::Null))
    }

    /// Kills the command and keeps the terminal so its output stays readable.
    pub fn kill(&self, id: &str) -> Result<Value, RpcError> {
        self.get(id)?.kill();
        Ok(json!({}))
    }

    /// Kills the command and forgets the terminal.
    pub fn release(&self, id: &str) -> Result<Value, RpcError> {
        let terminal = lock(&self.map).remove(id).ok_or_else(|| unknown(id))?;
        terminal.kill();
        Ok(json!({}))
    }

    /// Kills every command. Used when the session shuts down.
    pub fn release_all(&self) {
        for (_, terminal) in lock(&self.map).drain() {
            terminal.kill();
        }
    }

    fn get(&self, id: &str) -> Result<Arc<Terminal>, RpcError> {
        lock(&self.map).get(id).cloned().ok_or_else(|| unknown(id))
    }
}

impl Terminal {
    fn kill(&self) {
        if let Some(kill) = lock(&self.kill).take() {
            let _ = kill.send(());
        }
    }
}

async fn pump(mut from: impl AsyncRead + Unpin, into: Arc<Mutex<Output>>) {
    let mut buf = [0u8; 8192];
    loop {
        match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => lock(&into).push(&buf[..n]),
        }
    }
}

fn exit_json(status: ExitStatus) -> Value {
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status).map(signal_name);
    #[cfg(not(unix))]
    let signal: Option<String> = None;
    json!({ "exitCode": status.code(), "signal": signal })
}

#[cfg(unix)]
fn signal_name(signal: i32) -> String {
    match signal {
        1 => "SIGHUP".to_owned(),
        2 => "SIGINT".to_owned(),
        6 => "SIGABRT".to_owned(),
        9 => "SIGKILL".to_owned(),
        11 => "SIGSEGV".to_owned(),
        13 => "SIGPIPE".to_owned(),
        15 => "SIGTERM".to_owned(),
        other => format!("SIG{other}"),
    }
}

fn unknown(id: &str) -> RpcError {
    RpcError::remote(code::INVALID_PARAMS, format!("unknown terminal {id}"))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Each guarded value is updated in one call, so a panic can't leave it half written.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_keeps_the_tail_on_a_char_boundary() {
        let mut out = Output { bytes: Vec::new(), limit: Some(2), truncated: false };
        out.push("a".as_bytes());
        out.push("éb".as_bytes());
        // "aéb" is 4 bytes; keeping the last 2 would start inside "é", so only "b" stays.
        assert_eq!(String::from_utf8(out.bytes).unwrap(), "b");
        assert!(out.truncated);
    }
}
