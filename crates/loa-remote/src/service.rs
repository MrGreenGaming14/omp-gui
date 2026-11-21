//! `RemoteService`: host lookup, limits, approval, the SSH work, and one audit row per call
//! whatever the outcome ([`RemoteService::audited`]).

use loa_util::LockExt;
use loa_util::exec::shell_quote;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use russh_sftp::protocol::FileAttributes;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::audit::{Action, AuditSink, NewRecord, Outcome};
use crate::connection::{Connector, ConnectorConfig, Session};
use crate::error::{Error, Result};
use crate::exec::{self, ExecEnd, ExecOutput};
use crate::hosts::{Host, HostRegistry};
use crate::known_hosts::KnownHosts;
use crate::limits::{normalize, split, within};
use crate::ports::{Approval, ApprovalRequest, Approver, HostKeyDecider};
use crate::pty::{self, Shell};
use crate::sftp::{Sftp, WriteMode, is_server_status};

/// Session id audit rows of user opened terminals carry.
pub const TERMINAL_SESSION: &str = "terminal";

/// A `remote_bash` call; `timeout_secs` is capped by the host's `max_timeout_secs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashRequest {
    pub host: String,
    pub command: String,
    pub cwd: Option<String>,
    pub timeout_secs: Option<u64>,
}

/// What `remote_file` should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    Read,
    List,
    Write { content: Vec<u8> },
    /// Replace the one occurrence of `old` with `new`.
    Edit { old: String, new: String },
}

/// A `remote_file` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRequest {
    pub host: String,
    pub path: String,
    pub op: FileOp,
}

/// Kind of a directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// One entry of a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub mtime: Option<u32>,
    pub permissions: Option<u32>,
}

impl DirEntry {
    fn new(name: String, a: &FileAttributes) -> Self {
        let kind = match (a.is_symlink(), a.is_dir(), a.is_regular()) {
            (true, _, _) => EntryKind::Symlink,
            (_, true, _) => EntryKind::Dir,
            (_, _, true) => EntryKind::File,
            _ => EntryKind::Other,
        };
        Self { name, kind, size: a.size, mtime: a.mtime, permissions: a.permissions.map(|p| p & 0o7777) }
    }
}

/// Result of a `remote_file` call; `path` is the real path acted on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FileOutput {
    /// `encoding` is `utf8` or `base64`.
    Read { path: String, encoding: &'static str, content: String, size: u64 },
    List { path: String, entries: Vec<DirEntry> },
    Write { path: String, bytes: u64, mode: WriteMode },
    Edit { path: String, bytes: u64, mode: WriteMode },
}

/// What an audit row says about a successful result.
trait Summary {
    fn summary(&self) -> (Outcome, String);
}

impl Summary for ExecOutput {
    fn summary(&self) -> (Outcome, String) {
        let outcome = match self.end {
            ExecEnd::Exited => Outcome::Ok,
            ExecEnd::TimedOut => Outcome::TimedOut,
            ExecEnd::Cancelled => Outcome::Cancelled,
        };
        let status = match (&self.exit_status, &self.exit_signal) {
            (Some(s), _) => format!("exit {s}"),
            (None, Some(sig)) => format!("signal {sig}"),
            (None, None) => "no exit status".to_owned(),
        };
        let (out, err) = (self.stdout.len() as u64 + self.stdout_dropped, self.stderr.len() as u64 + self.stderr_dropped);
        (outcome, format!("{status}; stdout {out} bytes, stderr {err} bytes"))
    }
}

impl Summary for FileOutput {
    fn summary(&self) -> (Outcome, String) {
        let detail = match self {
            FileOutput::Read { size, .. } => format!("{size} bytes"),
            FileOutput::List { entries, .. } => format!("{} entries", entries.len()),
            FileOutput::Write { bytes, mode, .. } | FileOutput::Edit { bytes, mode, .. } => format!("{bytes} bytes, {mode:?}"),
        };
        (Outcome::Ok, detail)
    }
}

impl Summary for (Shell, String) {
    fn summary(&self) -> (Outcome, String) {
        (Outcome::Ok, self.1.clone())
    }
}

fn outcome_of(e: &Error) -> Outcome {
    match e {
        Error::Refused(_) | Error::UnknownHost(_) => Outcome::Refused,
        Error::Denied(_) => Outcome::Denied,
        Error::Cancelled => Outcome::Cancelled,
        Error::TimedOut => Outcome::TimedOut,
        _ => Outcome::Error,
    }
}

/// Replaces the one occurrence of `old` in `text` with `new`.
pub fn apply_edit(text: &str, old: &str, new: &str) -> std::result::Result<String, String> {
    if old.is_empty() {
        return Err("edit needs a non-empty `old` string".to_owned());
    }
    match text.match_indices(old).count() {
        0 => Err("`old` string not found".to_owned()),
        1 => Ok(text.replacen(old, new, 1)),
        n => Err(format!("`old` string occurs {n} times; it must be unique")),
    }
}

fn preview(s: &str) -> &str {
    s.char_indices().nth(200).map_or(s, |(i, _)| &s[..i])
}

/// Paths and settings the app passes in.
pub struct RemoteServiceConfig {
    pub known_hosts: Vec<PathBuf>,
    /// File accepted host keys are appended to.
    pub known_hosts_write: PathBuf,
    pub connector: ConnectorConfig,
}

impl RemoteServiceConfig {
    /// `~/.ssh/known_hosts`, `~/.ssh/known_hosts2`, the system `ssh_known_hosts` and the SSH agent.
    pub fn from_home(home: &Path) -> Self {
        let user = home.join(".ssh/known_hosts");
        let known_hosts = vec![user.clone(), home.join(".ssh/known_hosts2"), system_known_hosts(std::env::var_os("ProgramData"))];
        Self { known_hosts, known_hosts_write: user, connector: ConnectorConfig::from_env() }
    }
}

fn system_known_hosts(program_data: Option<std::ffi::OsString>) -> PathBuf {
    if cfg!(windows) {
        program_data
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join("ssh")
            .join("ssh_known_hosts")
    } else {
        PathBuf::from("/etc/ssh/ssh_known_hosts")
    }
}

#[cfg(all(test, windows))]
#[test]
fn openssh_system_known_hosts_uses_program_data() {
    assert_eq!(system_known_hosts(Some(r"D:\Data".into())), PathBuf::from(r"D:\Data\ssh\ssh_known_hosts"));
    assert_eq!(system_known_hosts(None), PathBuf::from(r"C:\ProgramData\ssh\ssh_known_hosts"));
}

/// The `remote` server's logic, shared by every session.
pub struct RemoteService {
    hosts: Arc<HostRegistry>,
    connector: Connector,
    audit: Arc<dyn AuditSink>,
    approver: Arc<dyn Approver>,
    /// "Allow for session" answers: (session, host, commands?).
    grants: Mutex<HashSet<(String, String, bool)>>,
}

impl RemoteService {
    pub fn new(
        hosts: Arc<HostRegistry>,
        audit: Arc<dyn AuditSink>,
        decider: Arc<dyn HostKeyDecider>,
        approver: Arc<dyn Approver>,
        config: RemoteServiceConfig,
    ) -> Self {
        let known = Arc::new(KnownHosts::new(config.known_hosts, config.known_hosts_write));
        let connector = Connector::new(known, decider, config.connector);
        Self { hosts, connector, audit, approver, grants: Mutex::default() }
    }

    pub fn hosts(&self) -> &Arc<HostRegistry> {
        &self.hosts
    }

    pub fn audit(&self) -> &Arc<dyn AuditSink> {
        &self.audit
    }

    /// Closes the pooled connection to a host.
    pub async fn disconnect(&self, host: &str) {
        self.connector.disconnect(host).await;
    }

    /// Forgets "allow for session" answers given in `session_id`.
    pub fn end_session(&self, session_id: &str) {
        self.grants.locked().retain(|(s, _, _)| s != session_id);
    }

    /// Records one audit row for `result` and passes it through.
    fn audited<T: Summary>(&self, session: &str, host: &str, action: Action, target: &str, result: Result<T>) -> Result<T> {
        let (outcome, detail) = match &result {
            Ok(value) => value.summary(),
            Err(e) => (outcome_of(e), e.to_string()),
        };
        let (host, session_id, target) = (host.to_owned(), session.to_owned(), target.to_owned());
        self.audit.record(NewRecord { at: SystemTime::now(), host, session_id, action, target, outcome, detail })?;
        result
    }

    fn host(&self, name: &str) -> Result<Host> {
        self.hosts.get(name).ok_or_else(|| Error::UnknownHost(name.to_owned()))
    }

    /// Asks the approval port unless the host or an earlier "allow for session" allows it.
    async fn approve(&self, session_id: &str, host: &Host, action: Action, target: &str, detail: String) -> Result<()> {
        if !host.limits.needs_approval(action) {
            return Ok(());
        }
        let key = (session_id.to_owned(), host.name.clone(), action == Action::Command);
        if self.grants.locked().contains(&key) {
            return Ok(());
        }
        let request = ApprovalRequest { session_id: session_id.to_owned(), host: host.name.clone(), action, target: target.to_owned(), detail };
        match self.approver.approve(request).await {
            Approval::AllowOnce => Ok(()),
            Approval::AllowSession => {
                self.grants.locked().insert(key);
                Ok(())
            }
            Approval::Deny { reason } => Err(Error::Denied(reason)),
        }
    }

    /// Runs a command on a host (`remote_bash`).
    pub async fn bash(&self, session_id: &str, request: BashRequest, cancel: &CancellationToken) -> Result<ExecOutput> {
        let command = match &request.cwd {
            Some(cwd) => format!("cd {} && {}", shell_quote(cwd), request.command),
            None => request.command.clone(),
        };
        let result = self.bash_inner(session_id, &request, &command, cancel).await;
        self.audited(session_id, &request.host, Action::Command, &command, result)
    }

    async fn bash_inner(&self, session_id: &str, request: &BashRequest, command: &str, cancel: &CancellationToken) -> Result<ExecOutput> {
        let host = self.host(&request.host)?;
        let limits = &host.limits;
        limits.check_action(Action::Command).map_err(Error::Refused)?;
        if request.command.trim().is_empty() {
            return Err(Error::InvalidRequest("command is empty".to_owned()));
        }
        if let Some(cwd) = &request.cwd {
            limits.check_path(&normalize(cwd).map_err(Error::Refused)?).map_err(Error::Refused)?;
        }
        let secs = request.timeout_secs.unwrap_or(limits.default_timeout_secs).min(limits.max_timeout_secs).max(1);
        let detail = match &request.cwd {
            Some(cwd) => format!("in {cwd}, timeout {secs}s"),
            None => format!("timeout {secs}s"),
        };
        let session = tokio::select! {
            r = async {
                self.approve(session_id, &host, Action::Command, &request.command, detail).await?;
                self.connector.session(&host).await
            } => r?,
            () = cancel.cancelled() => return Err(Error::Cancelled),
        };
        if let Some(cwd) = &request.cwd
            && !limits.allows_every_path()
        {
            real_target(&host, &*session.sftp().await?, cwd, false, &mut String::new()).await?;
        }
        exec::run(&session.handle, &host.name, command, limits.max_output_bytes, Duration::from_secs(secs), cancel).await
    }

    /// Opens an interactive login shell for the terminal pane; host keys are checked, limits and
    /// approvals don't apply (the user opened it). Audited under [`TERMINAL_SESSION`].
    pub async fn open_shell(&self, host: &str, cols: u16, rows: u16) -> Result<Shell> {
        let result = async { Ok((pty::open(self.connector.session(&self.host(host)?).await?, cols, rows).await?, format!("{cols}x{rows}"))) };
        let result = self.audited(TERMINAL_SESSION, host, Action::Terminal, "login shell", result.await);
        result.map(|(shell, _)| shell)
    }

    /// Reads, lists, writes or edits a file on a host (`remote_file`).
    pub async fn file(&self, session_id: &str, request: FileRequest, cancel: &CancellationToken) -> Result<FileOutput> {
        let action = match request.op {
            FileOp::Read => Action::Read,
            FileOp::List => Action::List,
            FileOp::Write { .. } => Action::Write,
            FileOp::Edit { .. } => Action::Edit,
        };
        let mut target = request.path.clone();
        let result = tokio::select! {
            r = self.file_inner(session_id, &request, action, &mut target) => r,
            () = cancel.cancelled() => Err(Error::Cancelled),
        };
        self.audited(session_id, &request.host, action, &target, result)
    }

    async fn file_inner(&self, session_id: &str, request: &FileRequest, action: Action, target: &mut String) -> Result<FileOutput> {
        let host = self.host(&request.host)?;
        host.limits.check_action(action).map_err(Error::Refused)?;
        let path = normalize(&request.path).map_err(Error::Refused)?;
        target.clone_from(&path);
        host.limits.check_path(&path).map_err(Error::Refused)?;
        let max = host.limits.max_file_bytes;
        match &request.op {
            FileOp::Write { content } if content.len() as u64 > max => return Err(Error::TooLarge { path, size: content.len() as u64, max }),
            FileOp::Edit { old, .. } if old.is_empty() => return Err(Error::InvalidRequest("edit needs a non-empty `old` string".to_owned())),
            _ => {}
        }
        let session = self.connector.session(&host).await?;
        let result = self.file_on(session_id, &host, &session, &request.op, &path, target).await;
        if let Err(e @ Error::Sftp { .. }) = &result
            && !is_server_status(e)
        {
            // The channel itself failed; open a fresh one next time.
            session.reset_sftp().await;
        }
        result
    }

    async fn file_on(&self, session_id: &str, host: &Host, session: &Session, op: &FileOp, path: &str, target: &mut String) -> Result<FileOutput> {
        let sftp = session.sftp().await?;
        let path = real_target(host, &sftp, path, matches!(op, FileOp::Write { .. }), target).await?;
        let max = host.limits.max_file_bytes;
        match op {
            FileOp::Read => {
                let bytes = sftp.read(&path, max).await?;
                let size = bytes.len() as u64;
                let (encoding, content) = match String::from_utf8(bytes) {
                    Ok(s) => ("utf8", s),
                    Err(e) => ("base64", STANDARD.encode(e.as_bytes())),
                };
                Ok(FileOutput::Read { path, encoding, content, size })
            }
            FileOp::List => {
                let entries = sftp.list(&path).await?.into_iter().map(|(name, attrs)| DirEntry::new(name, &attrs)).collect();
                Ok(FileOutput::List { path, entries })
            }
            FileOp::Write { content } => {
                self.approve(session_id, host, Action::Write, &path, format!("{} bytes", content.len())).await?;
                let mode = sftp.write(&path, content).await?;
                Ok(FileOutput::Write { bytes: content.len() as u64, path, mode })
            }
            FileOp::Edit { old, new } => {
                let edit_err = |message: String| Error::Edit { path: path.clone(), message };
                let text = String::from_utf8(sftp.read(&path, max).await?).map_err(|_| edit_err("file is not UTF-8 text".to_owned()))?;
                let edited = apply_edit(&text, old, new).map_err(edit_err)?;
                if edited.len() as u64 > max {
                    return Err(Error::TooLarge { path, size: edited.len() as u64, max });
                }
                self.approve(session_id, host, Action::Edit, &path, format!("replace {:?} with {:?}", preview(old), preview(new))).await?;
                let mode = sftp.write(&path, edited.as_bytes()).await?;
                Ok(FileOutput::Edit { bytes: edited.len() as u64, path, mode })
            }
        }
    }
}

/// Resolves `path` on the host and checks the real path against the (resolved) allowed prefixes,
/// so a symlink cannot lead outside them. A write to a missing file resolves its parent instead.
async fn real_target(host: &Host, sftp: &Sftp, path: &str, creating: bool, target: &mut String) -> Result<String> {
    let real = if creating && sftp.stat(path, true).await?.is_none() {
        let (dir, name) = split(path);
        let dir = sftp.realpath(dir).await?;
        format!("{}/{name}", dir.trim_end_matches('/'))
    } else {
        sftp.realpath(path).await?
    };
    let unusable = |e| Error::Sftp { host: host.name.clone(), message: format!("server returned an unusable real path: {e}") };
    let real = normalize(&real).map_err(unusable)?;
    if real != path {
        *target = format!("{path} -> {real}");
    }
    if host.limits.allows_every_path() {
        return Ok(real);
    }
    let mut prefixes = Vec::new();
    for prefix in host.limits.prefixes() {
        match sftp.realpath(&prefix).await {
            Ok(p) => prefixes.extend(normalize(&p).ok()),
            Err(e) if is_server_status(&e) => {}
            Err(e) => return Err(e),
        }
        prefixes.push(prefix);
    }
    match prefixes.iter().any(|p| within(&real, p)) {
        true => Ok(real),
        false => Err(Error::Refused(format!("{path} resolves to {real}, outside the allowed paths"))),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn edits_replace_exactly_one_occurrence() {
        let cases = [
            ("b", "X", Ok("aXc")),
            ("a", "", Ok("bc")),
            ("", "X", Err("edit needs a non-empty `old` string")),
            ("z", "X", Err("`old` string not found")),
            ("=", ":", Err("`old` string occurs 2 times; it must be unique")),
        ];
        for (old, new, want) in cases {
            let text = if old == "=" { "a=b=c" } else { "abc" };
            assert_eq!(super::apply_edit(text, old, new), want.map(String::from).map_err(String::from), "{old}");
        }
    }
}
