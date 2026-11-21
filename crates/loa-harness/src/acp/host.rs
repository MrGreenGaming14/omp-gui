//! The client methods agents call: `fs/*` and `terminal/*`, confined to the session cwd.

use super::wire::{CreateTerminalRequest, ReadTextFileRequest, TerminalRef, WriteTextFileRequest};
use crate::kit::RpcError;
use crate::kit::proc::TailBuffer;
use loa_util::LockExt;
use serde::de::DeserializeOwned;
use serde_json::{Value as Json, json};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{oneshot, watch};

const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL: i64 = -32603;
const RESOURCE_NOT_FOUND: i64 = -32002;

fn fail(code: i64, message: impl Into<String>) -> RpcError {
    RpcError::Command { command: "request".into(), message: message.into(), code: Some(code.to_string()) }
}

fn io_error(path: &str, err: &io::Error) -> RpcError {
    let code = match err.kind() {
        io::ErrorKind::PermissionDenied => INVALID_PARAMS,
        io::ErrorKind::NotFound => RESOURCE_NOT_FOUND,
        _ => INTERNAL,
    };
    fail(code, format!("{path}: {err}"))
}

fn params<T: DeserializeOwned>(params: Json) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| fail(INVALID_PARAMS, e.to_string()))
}

/// One session's files and terminals.
pub struct Host {
    /// The session cwd, canonical.
    root: PathBuf,
    terminals: Mutex<HashMap<String, Arc<Terminal>>>,
    next: AtomicU64,
}

struct Terminal {
    output: Arc<Mutex<TailBuffer>>,
    exit: watch::Receiver<Option<Json>>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
}

impl Terminal {
    fn kill(&self) {
        if let Some(kill) = self.kill.locked().take() {
            let _ = kill.send(());
        }
    }
}

impl Host {
    pub fn new(cwd: &Path) -> io::Result<Self> {
        Ok(Self { root: loa_util::fs::canonicalize(cwd)?, terminals: Mutex::default(), next: AtomicU64::new(1) })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `path` (absolute, or relative to the root) inside the root; `..`, and
    /// symlinks that lead out, are refused.
    pub fn resolve(&self, path: &str) -> io::Result<PathBuf> {
        let path = Path::new(path);
        let rel = match path.strip_prefix(&self.root) {
            Ok(rel) => rel,
            Err(_) if path.is_absolute() => return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} is outside the session directory", path.display()))),
            Err(_) => path,
        };
        loa_util::fs::confine(&self.root, rel)
    }

    /// Serves one request of `method`.
    pub async fn serve(&self, method: &str, request: Json) -> Result<Json, RpcError> {
        match method {
            "fs/read_text_file" => {
                let request: ReadTextFileRequest = params(request)?;
                let text = tokio::fs::read_to_string(self.resolve(&request.path).map_err(|e| io_error(&request.path, &e))?).await;
                let text = text.map_err(|e| io_error(&request.path, &e))?;
                let skip = request.line.map_or(0, |line| line.saturating_sub(1) as usize);
                let take = request.limit.map_or(usize::MAX, |limit| limit as usize);
                Ok(json!({ "content": text.split_inclusive('\n').skip(skip).take(take).collect::<String>() }))
            }
            "fs/write_text_file" => {
                let request: WriteTextFileRequest = params(request)?;
                let path = self.resolve(&request.path).map_err(|e| io_error(&request.path, &e))?;
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| io_error(&request.path, &e))?;
                }
                tokio::fs::write(&path, request.content).await.map_err(|e| io_error(&request.path, &e))?;
                Ok(Json::Null)
            }
            "terminal/create" => self.create(params(request)?),
            "terminal/output" => {
                let terminal = self.terminal(params(request)?)?;
                let exit = terminal.exit.borrow().clone();
                let output = terminal.output.locked();
                Ok(json!({ "output": output.text(), "truncated": output.truncated(), "exitStatus": exit }))
            }
            "terminal/wait_for_exit" => {
                let mut exit = self.terminal(params(request)?)?.exit.clone();
                let status = exit.wait_for(Option::is_some).await.map_err(|_| RpcError::Closed)?.clone();
                Ok(status.unwrap_or(Json::Null))
            }
            // `kill` keeps the terminal so its output stays readable; `release` forgets it.
            "terminal/kill" => self.terminal(params(request)?).map(|t| t.kill()).map(|()| json!({})),
            "terminal/release" => {
                let TerminalRef { terminal_id } = params(request)?;
                let terminal = self.terminals.locked().remove(&terminal_id).ok_or_else(|| unknown(&terminal_id))?;
                terminal.kill();
                Ok(json!({}))
            }
            other => Err(fail(METHOD_NOT_FOUND, format!("method not found: {other}"))),
        }
    }

    fn create(&self, request: CreateTerminalRequest) -> Result<Json, RpcError> {
        let cwd = match &request.cwd {
            Some(cwd) => self.resolve(cwd).map_err(|e| io_error(cwd, &e))?,
            None => self.root.clone(),
        };
        let mut command = tokio::process::Command::new(loa_util::exec::resolve(Path::new(&request.command)));
        command.args(request.args.unwrap_or_default()).current_dir(&cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        command.envs(request.env.unwrap_or_default().into_iter().map(|var| (var.name, var.value)));
        let start_failed = |e| fail(INTERNAL, format!("can't run `{}`: {e}", request.command));
        // Windows: a job object, so a kill also ends what the command spawned.
        #[cfg(windows)]
        let (mut child, stdout, stderr) = {
            use process_wrap::tokio::{CommandWrap, JobObject, KillOnDrop};
            let mut command = CommandWrap::from(command);
            command.wrap(KillOnDrop).wrap(JobObject);
            let mut child = command.spawn().map_err(start_failed)?;
            let (stdout, stderr) = (child.stdout().take(), child.stderr().take());
            (child, stdout, stderr)
        };
        #[cfg(not(windows))]
        let (mut child, stdout, stderr) = {
            let mut child = command.spawn().map_err(start_failed)?;
            let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
            (child, stdout, stderr)
        };
        let limit = request.output_byte_limit.map(|l| usize::try_from(l).unwrap_or(usize::MAX));
        let output = Arc::new(Mutex::new(TailBuffer::new(limit)));
        let readers = [stdout.map(|out| pump(out, &output)), stderr.map(|err| pump(err, &output))];
        let (exit_tx, exit) = watch::channel(None);
        let (kill, killed) = oneshot::channel::<()>();
        tokio::spawn(async move {
            // Windows: the job's own wait would also wait for background
            // helpers, so wait for the command itself and end the job after.
            #[cfg(windows)]
            let status = {
                let status = tokio::select! {
                    status = child.inner_mut().wait() => status,
                    _ = killed => {
                        let _ = child.start_kill();
                        child.inner_mut().wait().await
                    }
                };
                drop(child);
                status
            };
            #[cfg(not(windows))]
            let status = tokio::select! {
                status = child.wait() => status,
                _ = killed => {
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
        self.terminals.locked().insert(id.clone(), Arc::new(Terminal { output, exit, kill: Mutex::new(Some(kill)) }));
        Ok(json!({ "terminalId": id }))
    }

    fn terminal(&self, TerminalRef { terminal_id }: TerminalRef) -> Result<Arc<Terminal>, RpcError> {
        self.terminals.locked().get(&terminal_id).cloned().ok_or_else(|| unknown(&terminal_id))
    }

    /// Kills every command; the session is gone.
    pub fn release_all(&self) {
        self.terminals.locked().drain().for_each(|(_, terminal)| terminal.kill());
    }
}

fn pump(mut from: impl AsyncRead + Unpin + Send + 'static, into: &Arc<Mutex<TailBuffer>>) -> tokio::task::JoinHandle<()> {
    let into = into.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        while let Ok(n @ 1..) = from.read(&mut buf).await {
            into.locked().push(&buf[..n]);
        }
    })
}

fn exit_json(status: std::process::ExitStatus) -> Json {
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status).map(|signal| match signal {
        1 => "SIGHUP".to_owned(),
        2 => "SIGINT".to_owned(),
        6 => "SIGABRT".to_owned(),
        9 => "SIGKILL".to_owned(),
        11 => "SIGSEGV".to_owned(),
        13 => "SIGPIPE".to_owned(),
        15 => "SIGTERM".to_owned(),
        other => format!("SIG{other}"),
    });
    #[cfg(not(unix))]
    let signal: Option<String> = None;
    json!({ "exitCode": status.code(), "signal": signal })
}

fn unknown(id: &str) -> RpcError {
    fail(INVALID_PARAMS, format!("unknown terminal {id}"))
}
