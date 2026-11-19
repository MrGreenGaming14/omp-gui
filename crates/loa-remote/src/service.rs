//! `RemoteService`: the logic behind `remote_bash` and `remote_file`.
//!
//! Every call runs: host lookup, limits (#100), approval, the SSH work, and
//! an audit row (#101) whatever the outcome.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use russh_sftp::protocol::FileAttributes;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::audit::{Action, AuditLog, NewRecord, Outcome};
use crate::connection::{Connector, ConnectorConfig, Session};
use crate::error::{Error, Result};
use crate::exec::{self, ExecEnd, ExecOutput};
use crate::hosts::{Host, HostRegistry};
use crate::known_hosts::KnownHosts;
use crate::limits::{self, file_name, parent};
use crate::ports::{Approval, ApprovalRequest, Approver, HostKeyDecider};
use crate::sftp::{Sftp, WriteMode, is_server_status};

/// A `remote_bash` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashRequest {
    pub host: String,
    pub command: String,
    /// Absolute directory to run in.
    pub cwd: Option<String>,
    /// Seconds; capped by the host's `max_timeout_secs`.
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

impl FileOp {
    fn action(&self) -> Action {
        match self {
            FileOp::Read => Action::Read,
            FileOp::List => Action::List,
            FileOp::Write { .. } => Action::Write,
            FileOp::Edit { .. } => Action::Edit,
        }
    }
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
    fn new(name: String, attrs: &FileAttributes) -> Self {
        let kind = if attrs.is_symlink() {
            EntryKind::Symlink
        } else if attrs.is_dir() {
            EntryKind::Dir
        } else if attrs.is_regular() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        Self {
            name,
            kind,
            size: attrs.size,
            mtime: attrs.mtime,
            permissions: attrs.permissions.map(|p| p & 0o7777),
        }
    }
}

/// Result of a `remote_file` call. `path` is the real path acted on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum FileOutput {
    Read {
        path: String,
        /// `utf8` or `base64`.
        encoding: &'static str,
        content: String,
        size: u64,
    },
    List {
        path: String,
        entries: Vec<DirEntry>,
    },
    Write {
        path: String,
        bytes: u64,
        mode: WriteMode,
    },
    Edit {
        path: String,
        bytes: u64,
        mode: WriteMode,
    },
}

/// Paths and settings the app passes in.
pub struct RemoteServiceConfig {
    pub known_hosts: Vec<PathBuf>,
    /// File accepted host keys are appended to.
    pub known_hosts_write: PathBuf,
    pub connector: ConnectorConfig,
}

impl RemoteServiceConfig {
    /// The usual OpenSSH files under `home` (`~/.ssh/known_hosts`,
    /// `~/.ssh/known_hosts2`, `/etc/ssh/ssh_known_hosts`) and `SSH_AUTH_SOCK`.
    pub fn from_home(home: &std::path::Path) -> Self {
        let user = home.join(".ssh/known_hosts");
        Self {
            known_hosts: vec![
                user.clone(),
                home.join(".ssh/known_hosts2"),
                PathBuf::from("/etc/ssh/ssh_known_hosts"),
            ],
            known_hosts_write: user,
            connector: ConnectorConfig::from_env(),
        }
    }
}

/// Approval classes remembered for a session after "allow for session".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Class {
    Writes,
    Commands,
}

fn class(action: Action) -> Option<Class> {
    match action {
        Action::Write | Action::Edit => Some(Class::Writes),
        Action::Command => Some(Class::Commands),
        Action::Read | Action::List => None,
    }
}

/// The `remote` server's logic, shared by every session.
pub struct RemoteService {
    hosts: Arc<HostRegistry>,
    connector: Connector,
    audit: Arc<AuditLog>,
    approver: Arc<dyn Approver>,
    session_grants: Mutex<HashSet<(String, String, Class)>>,
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

fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn preview(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

impl RemoteService {
    pub fn new(
        hosts: Arc<HostRegistry>,
        audit: Arc<AuditLog>,
        decider: Arc<dyn HostKeyDecider>,
        approver: Arc<dyn Approver>,
        config: RemoteServiceConfig,
    ) -> Self {
        let known = Arc::new(KnownHosts::new(config.known_hosts, config.known_hosts_write));
        Self {
            hosts,
            connector: Connector::new(known, decider, config.connector),
            audit,
            approver,
            session_grants: Mutex::new(HashSet::new()),
        }
    }

    pub fn hosts(&self) -> &Arc<HostRegistry> {
        &self.hosts
    }

    pub fn audit(&self) -> &Arc<AuditLog> {
        &self.audit
    }

    /// Closes the pooled connection to a host.
    pub async fn disconnect(&self, host: &str) {
        self.connector.disconnect(host).await;
    }

    /// Forgets "allow for session" answers given in `session_id`.
    pub fn end_session(&self, session_id: &str) {
        self.grants().retain(|(s, _, _)| s != session_id);
    }

    fn grants(&self) -> std::sync::MutexGuard<'_, HashSet<(String, String, Class)>> {
        // Holds plain keys; a poisoned set is still consistent.
        self.session_grants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn record(
        &self,
        host: &str,
        session_id: &str,
        action: Action,
        target: &str,
        outcome: Outcome,
        detail: &str,
    ) -> Result<()> {
        self.audit.record(&NewRecord {
            at: SystemTime::now(),
            host,
            session_id,
            action,
            target,
            outcome,
            detail,
        })?;
        Ok(())
    }

    fn host(&self, name: &str) -> Result<Host> {
        self.hosts
            .get(name)
            .ok_or_else(|| Error::UnknownHost(name.to_string()))
    }

    /// Asks the approval port unless the host or the session allows it.
    async fn approve(
        &self,
        session_id: &str,
        host: &Host,
        action: Action,
        target: &str,
        detail: String,
    ) -> Result<()> {
        if !host.limits.needs_approval(action) {
            return Ok(());
        }
        let Some(class) = class(action) else {
            return Ok(());
        };
        let key = (session_id.to_string(), host.name.clone(), class);
        if self.grants().contains(&key) {
            return Ok(());
        }
        let answer = self
            .approver
            .approve(ApprovalRequest {
                session_id: session_id.to_string(),
                host: host.name.clone(),
                action,
                target: target.to_string(),
                detail,
            })
            .await;
        match answer {
            Approval::AllowOnce => Ok(()),
            Approval::AllowSession => {
                self.grants().insert(key);
                Ok(())
            }
            Approval::Deny { reason } => Err(Error::Denied(reason)),
        }
    }

    /// Runs a command on a host (`remote_bash`).
    pub async fn bash(
        &self,
        session_id: &str,
        request: BashRequest,
        cancel: &CancellationToken,
    ) -> Result<ExecOutput> {
        let target = match &request.cwd {
            Some(cwd) => format!("cd {} && {}", shell_quote(cwd), request.command),
            None => request.command.clone(),
        };
        let result = self.bash_inner(session_id, &request, &target, cancel).await;
        let (outcome, detail) = match &result {
            Ok(out) => {
                let outcome = match out.end {
                    ExecEnd::Exited => Outcome::Ok,
                    ExecEnd::TimedOut => Outcome::TimedOut,
                    ExecEnd::Cancelled => Outcome::Cancelled,
                };
                let status = match (&out.exit_status, &out.exit_signal) {
                    (Some(s), _) => format!("exit {s}"),
                    (None, Some(sig)) => format!("signal {sig}"),
                    (None, None) => "no exit status".to_string(),
                };
                (
                    outcome,
                    format!(
                        "{status}; stdout {} bytes, stderr {} bytes",
                        out.stdout.len() as u64 + out.stdout_dropped,
                        out.stderr.len() as u64 + out.stderr_dropped
                    ),
                )
            }
            Err(e) => (outcome_of(e), e.to_string()),
        };
        self.record(
            &request.host,
            session_id,
            Action::Command,
            &target,
            outcome,
            &detail,
        )?;
        result
    }

    async fn bash_inner(
        &self,
        session_id: &str,
        request: &BashRequest,
        command: &str,
        cancel: &CancellationToken,
    ) -> Result<ExecOutput> {
        let host = self.host(&request.host)?;
        host.limits
            .check_action(Action::Command)
            .map_err(Error::Refused)?;
        if request.command.trim().is_empty() {
            return Err(Error::InvalidRequest("command is empty".to_string()));
        }
        if let Some(cwd) = &request.cwd {
            let cwd = limits::normalize(cwd).map_err(Error::Refused)?;
            host.limits.check_path(&cwd).map_err(Error::Refused)?;
        }
        let limits = &host.limits;
        let timeout = Duration::from_secs(
            request
                .timeout_secs
                .unwrap_or(limits.default_timeout_secs)
                .min(limits.max_timeout_secs)
                .max(1),
        );
        let detail = match &request.cwd {
            Some(cwd) => format!("in {cwd}, timeout {}s", timeout.as_secs()),
            None => format!("timeout {}s", timeout.as_secs()),
        };
        tokio::select! {
            r = self.approve(session_id, &host, Action::Command, &request.command, detail) => r?,
            () = cancel.cancelled() => return Err(Error::Cancelled),
        }
        let session = self.connect(&host, cancel).await?;
        if let Some(cwd) = &request.cwd
            && !host.limits.allowed_paths.is_empty()
        {
            let sftp = session.sftp().await?;
            self.real_target(&host, &sftp, cwd, false, &mut String::new()).await?;
        }
        exec::run(
            &session.handle,
            &host.name,
            command,
            limits.max_output_bytes,
            timeout,
            cancel,
        )
        .await
    }

    async fn connect(&self, host: &Host, cancel: &CancellationToken) -> Result<Arc<Session>> {
        tokio::select! {
            s = self.connector.session(host) => s,
            () = cancel.cancelled() => Err(Error::Cancelled),
        }
    }

    /// Reads, lists, writes or edits a file on a host (`remote_file`).
    pub async fn file(
        &self,
        session_id: &str,
        request: FileRequest,
        cancel: &CancellationToken,
    ) -> Result<FileOutput> {
        let action = request.op.action();
        let mut target = request.path.clone();
        let result = tokio::select! {
            r = self.file_inner(session_id, &request, &mut target) => r,
            () = cancel.cancelled() => Err(Error::Cancelled),
        };
        let (outcome, detail) = match &result {
            Ok(FileOutput::Read { size, .. }) => (Outcome::Ok, format!("{size} bytes")),
            Ok(FileOutput::List { entries, .. }) => {
                (Outcome::Ok, format!("{} entries", entries.len()))
            }
            Ok(FileOutput::Write { bytes, mode, .. } | FileOutput::Edit { bytes, mode, .. }) => {
                (Outcome::Ok, format!("{bytes} bytes, {mode:?}"))
            }
            Err(e) => (outcome_of(e), e.to_string()),
        };
        self.record(&request.host, session_id, action, &target, outcome, &detail)?;
        result
    }

    async fn file_inner(
        &self,
        session_id: &str,
        request: &FileRequest,
        target: &mut String,
    ) -> Result<FileOutput> {
        let action = request.op.action();
        let host = self.host(&request.host)?;
        host.limits.check_action(action).map_err(Error::Refused)?;
        let path = limits::normalize(&request.path).map_err(Error::Refused)?;
        target.clone_from(&path);
        host.limits.check_path(&path).map_err(Error::Refused)?;
        if let FileOp::Write { content } = &request.op
            && content.len() as u64 > host.limits.max_file_bytes
        {
            return Err(Error::TooLarge {
                path,
                size: content.len() as u64,
                max: host.limits.max_file_bytes,
            });
        }
        if let FileOp::Edit { old, .. } = &request.op
            && old.is_empty()
        {
            return Err(Error::InvalidRequest(
                "edit needs a non-empty `old` string".to_string(),
            ));
        }

        let session = self.connector.session(&host).await?;
        let sftp = session.sftp().await?;
        let result = self
            .file_on(session_id, &host, &sftp, &request.op, &path, target)
            .await;
        if let Err(e) = &result
            && matches!(e, Error::Sftp { .. })
            && !is_server_status(e)
        {
            // The channel itself failed; open a fresh one next time.
            session.reset_sftp().await;
        }
        result
    }

    async fn file_on(
        &self,
        session_id: &str,
        host: &Host,
        sftp: &Sftp,
        op: &FileOp,
        path: &str,
        target: &mut String,
    ) -> Result<FileOutput> {
        let real = self
            .real_target(host, sftp, path, matches!(op, FileOp::Write { .. }), target)
            .await?;
        let max = host.limits.max_file_bytes;
        match op {
            FileOp::Read => {
                let bytes = sftp.read(&real, max).await?;
                let size = bytes.len() as u64;
                let (encoding, content) = match String::from_utf8(bytes) {
                    Ok(s) => ("utf8", s),
                    Err(e) => ("base64", STANDARD.encode(e.as_bytes())),
                };
                Ok(FileOutput::Read {
                    path: real,
                    encoding,
                    content,
                    size,
                })
            }
            FileOp::List => {
                let entries = sftp
                    .list(&real)
                    .await?
                    .into_iter()
                    .map(|(name, attrs)| DirEntry::new(name, &attrs))
                    .collect();
                Ok(FileOutput::List {
                    path: real,
                    entries,
                })
            }
            FileOp::Write { content } => {
                self.approve(
                    session_id,
                    host,
                    Action::Write,
                    &real,
                    format!("{} bytes", content.len()),
                )
                .await?;
                let mode = sftp.write(&real, content).await?;
                Ok(FileOutput::Write {
                    path: real,
                    bytes: content.len() as u64,
                    mode,
                })
            }
            FileOp::Edit { old, new } => {
                let bytes = sftp.read(&real, max).await?;
                let text = String::from_utf8(bytes).map_err(|_| Error::Edit {
                    path: real.clone(),
                    message: "file is not UTF-8 text".to_string(),
                })?;
                let mut found = text.match_indices(old.as_str());
                let at = match (found.next(), found.next()) {
                    (Some((at, _)), None) => at,
                    (None, _) => {
                        return Err(Error::Edit {
                            path: real,
                            message: "`old` string not found".to_string(),
                        });
                    }
                    (Some(_), Some(_)) => {
                        return Err(Error::Edit {
                            path: real,
                            message: format!(
                                "`old` string occurs {} times; it must be unique",
                                2 + found.count()
                            ),
                        });
                    }
                };
                let mut edited = String::with_capacity(text.len() - old.len() + new.len());
                edited.push_str(&text[..at]);
                edited.push_str(new);
                edited.push_str(&text[at + old.len()..]);
                if edited.len() as u64 > max {
                    return Err(Error::TooLarge {
                        path: real,
                        size: edited.len() as u64,
                        max,
                    });
                }
                self.approve(
                    session_id,
                    host,
                    Action::Edit,
                    &real,
                    format!(
                        "replace {:?} with {:?}",
                        preview(old, 200),
                        preview(new, 200)
                    ),
                )
                .await?;
                let mode = sftp.write(&real, edited.as_bytes()).await?;
                Ok(FileOutput::Edit {
                    path: real,
                    bytes: edited.len() as u64,
                    mode,
                })
            }
        }
    }

    /// Resolves `path` on the host and checks the real path against the
    /// allowed prefixes (themselves resolved), so a symlink inside an
    /// allowed directory cannot lead outside it.
    ///
    /// For writes the target may not exist yet: its parent is resolved
    /// instead, and an existing symlink is followed so the write lands on
    /// the file it points to rather than replacing the link.
    async fn real_target(
        &self,
        host: &Host,
        sftp: &Sftp,
        path: &str,
        creating: bool,
        target: &mut String,
    ) -> Result<String> {
        let real = if creating && sftp.stat(path).await?.is_none() {
            let dir = sftp.realpath(parent(path)).await?;
            let sep = if dir.ends_with('/') { "" } else { "/" };
            format!("{dir}{sep}{}", file_name(path))
        } else {
            sftp.realpath(path).await?
        };
        let real = limits::normalize(&real).map_err(|e| Error::Sftp {
            host: host.name.clone(),
            message: format!("server returned an unusable real path: {e}"),
        })?;
        if real != path {
            *target = format!("{path} -> {real}");
        }
        if host.limits.allowed_paths.is_empty() {
            return Ok(real);
        }
        let mut prefixes = Vec::new();
        for prefix in host.limits.prefixes() {
            match sftp.realpath(&prefix).await {
                Ok(p) => prefixes.extend(limits::normalize(&p).ok()),
                Err(e) if is_server_status(&e) => {}
                Err(e) => return Err(e),
            }
            prefixes.push(prefix);
        }
        if host.limits.allowed_by_any(&real, &prefixes) {
            Ok(real)
        } else {
            Err(Error::Refused(format!(
                "{path} resolves to {real}, outside the allowed paths"
            )))
        }
    }
}

/// Arguments of `remote_file` as the MCP tool receives them.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub(crate) struct FileArgs {
    /// Host name from the Remote hosts list.
    pub host: String,
    /// What to do.
    pub op: FileOpName,
    /// Absolute path on the host.
    pub path: String,
    /// `write`: the new file content.
    #[serde(default)]
    pub content: Option<String>,
    /// `write`: how `content` is encoded. Defaults to `utf8`.
    #[serde(default)]
    pub encoding: Option<Encoding>,
    /// `edit`: exact text to replace; must occur exactly once.
    #[serde(default)]
    pub old: Option<String>,
    /// `edit`: replacement text.
    #[serde(default)]
    pub new: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FileOpName {
    Read,
    Write,
    List,
    Edit,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Encoding {
    Utf8,
    Base64,
}

impl FileArgs {
    pub(crate) fn into_request(self) -> Result<FileRequest> {
        let missing = |field: &str, op: &str| {
            Error::InvalidRequest(format!("`{field}` is required for op `{op}`"))
        };
        let op = match self.op {
            FileOpName::Read => FileOp::Read,
            FileOpName::List => FileOp::List,
            FileOpName::Write => {
                let content = self.content.ok_or_else(|| missing("content", "write"))?;
                let content = match self.encoding.unwrap_or(Encoding::Utf8) {
                    Encoding::Utf8 => content.into_bytes(),
                    Encoding::Base64 => STANDARD.decode(content.as_bytes()).map_err(|e| {
                        Error::InvalidRequest(format!("`content` is not valid base64: {e}"))
                    })?,
                };
                FileOp::Write { content }
            }
            FileOpName::Edit => FileOp::Edit {
                old: self.old.ok_or_else(|| missing("old", "edit"))?,
                new: self.new.ok_or_else(|| missing("new", "edit"))?,
            },
        };
        Ok(FileRequest {
            host: self.host,
            path: self.path,
            op,
        })
    }
}

/// Arguments of `remote_bash` as the MCP tool receives them.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub(crate) struct BashArgs {
    /// Host name from the Remote hosts list.
    pub host: String,
    /// Shell command, run by the remote user's login shell.
    pub command: String,
    /// Absolute directory to run the command in.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Seconds before the command is stopped.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl From<BashArgs> for BashRequest {
    fn from(a: BashArgs) -> Self {
        Self {
            host: a.host,
            command: a.command,
            cwd: a.cwd,
            timeout_secs: a.timeout_secs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_survives_single_quotes() {
        assert_eq!(shell_quote("/srv/it's"), "'/srv/it'\\''s'");
    }

    #[test]
    fn file_args_validation() {
        let args: FileArgs = serde_json::from_value(serde_json::json!({
            "host": "h", "op": "write", "path": "/x", "content": "aGk=", "encoding": "base64"
        }))
        .unwrap();
        assert_eq!(
            args.into_request().unwrap().op,
            FileOp::Write {
                content: b"hi".to_vec()
            }
        );
        let args: FileArgs = serde_json::from_value(serde_json::json!({
            "host": "h", "op": "edit", "path": "/x", "old": "a"
        }))
        .unwrap();
        assert!(args.into_request().is_err());
    }
}
