//! `remote_bash` and `remote_file` as typed [`Tool`]s; [`tool_set`] serves them as host tools and,
//! through `ToolSetMcp`, over MCP.

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use loa_contracts::tools::{CallCtx, Tool, ToolAnnotations, ToolOutput, ToolSet};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};
use crate::service::{BashRequest, FileOp, FileRequest, RemoteService};

/// MCP server name.
pub const SERVER_NAME: &str = "remote";
pub const REMOTE_BASH: &str = "remote_bash";
pub const REMOTE_FILE: &str = "remote_file";

/// Arguments of `remote_bash`.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct BashArgs {
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

/// What `remote_file` does.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileOpName {
    Read,
    Write,
    List,
    Edit,
}

/// How `content` is encoded.
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    #[default]
    Utf8,
    Base64,
}

/// Arguments of `remote_file`.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct FileArgs {
    /// Host name from the Remote hosts list.
    pub host: String,
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

impl FileArgs {
    pub fn into_request(self) -> std::result::Result<FileRequest, String> {
        let missing = |field: &str, op: &str| format!("`{field}` is required for op `{op}`");
        let op = match self.op {
            FileOpName::Read => FileOp::Read,
            FileOpName::List => FileOp::List,
            FileOpName::Write => {
                let content = self.content.ok_or_else(|| missing("content", "write"))?;
                let content = match self.encoding.unwrap_or_default() {
                    Encoding::Utf8 => content.into_bytes(),
                    Encoding::Base64 => STANDARD.decode(content).map_err(|e| format!("`content` is not valid base64: {e}"))?,
                };
                FileOp::Write { content }
            }
            FileOpName::Edit => FileOp::Edit { old: self.old.ok_or_else(|| missing("old", "edit"))?, new: self.new.ok_or_else(|| missing("new", "edit"))? },
        };
        Ok(FileRequest { host: self.host, path: self.path, op })
    }
}

/// Runs `work` in its own task; dropping the call cancels its token, so the cancellation is still audited.
async fn detached<T, F>(work: impl FnOnce(CancellationToken) -> F) -> Result<T>
where
    T: Send + 'static,
    F: Future<Output = Result<T>> + Send + 'static,
{
    let cancel = CancellationToken::new();
    let _guard = cancel.clone().drop_guard();
    tokio::spawn(work(cancel)).await.unwrap_or(Err(Error::Cancelled))
}

/// The JSON the model reads; an error result when `failed` or on `Err`.
fn output<T: Serialize>(result: Result<T>, failed: impl FnOnce(&T) -> bool) -> std::result::Result<ToolOutput, String> {
    Ok(match result {
        Ok(value) => {
            let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
            if failed(&value) { ToolOutput::error(text) } else { ToolOutput::text(text) }
        }
        Err(e) => ToolOutput::error(e.to_string()),
    })
}

const HINTS: ToolAnnotations = ToolAnnotations { title: None, read_only: false, destructive: true };

/// `remote_bash`.
pub struct RemoteBash(pub Arc<RemoteService>);

#[async_trait]
impl Tool for RemoteBash {
    type Args = BashArgs;
    const NAME: &'static str = REMOTE_BASH;

    fn description(&self) -> &str {
        "Run a shell command on a configured SSH host. Returns stdout, stderr and the exit status. Subject to the host's limits and the user's approval."
    }

    fn annotations(&self) -> ToolAnnotations {
        HINTS
    }

    async fn run(&self, ctx: &CallCtx, a: BashArgs) -> std::result::Result<ToolOutput, String> {
        let (service, session) = (self.0.clone(), ctx.session_id.clone());
        let request = BashRequest { host: a.host, command: a.command, cwd: a.cwd, timeout_secs: a.timeout_secs };
        let result = detached(|cancel| async move { service.bash(&session, request, &cancel).await }).await;
        output(result, |out| out.exit_status != Some(0))
    }
}

/// `remote_file`.
pub struct RemoteFile(pub Arc<RemoteService>);

#[async_trait]
impl Tool for RemoteFile {
    type Args = FileArgs;
    const NAME: &'static str = REMOTE_FILE;

    fn description(&self) -> &str {
        "Read, list, write or edit a file on a configured SSH host. `edit` replaces one exact, unique occurrence of `old` with `new`. Writes are atomic where the server allows."
    }

    fn annotations(&self) -> ToolAnnotations {
        HINTS
    }

    async fn run(&self, ctx: &CallCtx, args: FileArgs) -> std::result::Result<ToolOutput, String> {
        let (service, session, request) = (self.0.clone(), ctx.session_id.clone(), args.into_request()?);
        output(detached(|cancel| async move { service.file(&session, request, &cancel).await }).await, |_| false)
    }
}

/// Both tools over `service`.
pub fn tool_set(service: Arc<RemoteService>) -> ToolSet {
    let mut set = ToolSet::new();
    set.register(RemoteBash(service.clone())).register(RemoteFile(service));
    set
}
