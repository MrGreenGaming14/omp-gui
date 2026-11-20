//! The worker session of a `sandbox_task` and the tools it works with.
//!
//! The worker runs on the host through a harness [`Adapter`], so its model
//! calls, API keys and journal never leave the host. Its harness gets no file
//! or exec tools of its own and no MCP servers; the only way it can act is
//! through the `sandbox_*` host tools here, each of which is one request to
//! the stub inside the sandbox.

use crate::client::StubClient;
use crate::proto::{Command, ExecRequest, FileKind};
use crate::{Result, SandboxError};
use async_trait::async_trait;
use loa_adapter::{Adapter, Answer, HostToolSpec, HostTools, Prompt, SessionConfig};
use loa_core::{
    CommitError, DecisionKind, DecisionStatus, Dom, Draft, ElementKind, MemorySink, MessageKind, NodeId, Op, Patch, PatchSink, Role,
    answer, attr,
};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

/// Names of the tools offered to the worker.
pub const SANDBOX_BASH: &str = "sandbox_bash";
pub const SANDBOX_READ: &str = "sandbox_read";
pub const SANDBOX_WRITE: &str = "sandbox_write";
pub const SANDBOX_LIST: &str = "sandbox_list";

/// Tools of the harness itself that act on the host. They are switched off
/// for a worker so everything it does goes through the sandbox.
pub const HOST_TOOLS_TO_DISABLE: &[&str] = &[
    "bash", "read", "write", "edit", "grep", "find", "glob", "ast_grep", "ast_edit", "eval", "lsp", "debug", "browser",
    "computer", "task", "fetch", "notebook", "ssh", "ask",
];

/// Where a worker's session is journaled. A port: `loa-app` backs it with the
/// bus and journal, so the worker's journal lives on the host like any other
/// session's.
pub trait WorkerJournal: Send + Sync {
    /// The sink of a new, empty session.
    fn open(&self, session_id: &str) -> Result<Arc<dyn PatchSink>>;
}

/// Keeps worker sessions in memory, for tests and previews.
#[derive(Default)]
pub struct MemoryJournal {
    sessions: Mutex<Vec<(String, Arc<MemorySink>)>>,
}

impl MemoryJournal {
    /// The sink of a session opened earlier.
    pub fn session(&self, session_id: &str) -> Option<Arc<MemorySink>> {
        let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        sessions.iter().find(|(id, _)| id == session_id).map(|(_, sink)| Arc::clone(sink))
    }
}

impl WorkerJournal for MemoryJournal {
    fn open(&self, session_id: &str) -> Result<Arc<dyn PatchSink>> {
        let sink = Arc::new(MemorySink::new());
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        sessions.push((session_id.to_owned(), Arc::clone(&sink)));
        Ok(sink)
    }
}

/// Forwards commits to the journal sink and keeps a copy of the DOM, so the
/// worker's final message can be read whatever the journal is. Approval
/// requests the worker's harness raises are handed to `approvals`: nobody
/// watches a worker, and its tools only reach the sandbox the user already
/// approved with the `sandbox_task` call.
struct Mirror {
    inner: Arc<dyn PatchSink>,
    dom: Mutex<Dom>,
    approvals: tokio::sync::mpsc::UnboundedSender<NodeId>,
}

impl PatchSink for Mirror {
    fn commit_at(&self, draft: Draft, ts: i64) -> Result<Patch, CommitError> {
        // Held across the commit so patches reach the mirror in sequence.
        let mut dom = self.dom.lock().unwrap_or_else(PoisonError::into_inner);
        let patch = self.inner.commit_at(draft, ts)?;
        dom.apply(&patch)?;
        let touches_status = match &patch.op {
            Op::Insert { kind, .. } => *kind == ElementKind::Decision,
            Op::Set { name, .. } => name == attr::STATUS,
            _ => false,
        };
        let waiting_approval = touches_status
            && dom.get(patch.node).is_some_and(|n| {
                n.kind == ElementKind::Decision
                    && n.str_attr(attr::KIND) == Some(DecisionKind::Approval.as_str())
                    && n.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str())
            });
        if waiting_approval {
            let _ = self.approvals.send(patch.node);
        }
        Ok(patch)
    }
}

/// The text of the last assistant message that isn't thinking.
pub fn final_message(dom: &Dom) -> Option<String> {
    dom.iter()
        .filter(|n| {
            n.kind == ElementKind::Message
                && n.str_attr(attr::ROLE) == Some(Role::Assistant.as_str())
                && n.str_attr(attr::KIND) != Some(MessageKind::Thinking.as_str())
        })
        .last()
        .map(|n| n.text.clone())
}

/// The `sandbox_*` tools, each answered by the stub.
pub struct SandboxTools {
    client: Arc<StubClient>,
    /// Sent with every command: providers that boot a VM can't set the
    /// stub's own environment.
    env: Vec<(String, String)>,
    /// Most bytes of output returned to the model per stream.
    output_limit: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BashArgs {
    command: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    limit: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    path: Option<String>,
}

fn parse<'a, T: Deserialize<'a>>(args: &'a str) -> Result<T, String> {
    serde_json::from_str(args).map_err(|e| format!("invalid arguments: {e}"))
}

impl SandboxTools {
    pub fn new(client: Arc<StubClient>, env: Vec<(String, String)>) -> Self {
        Self { client, env, output_limit: 64 * 1024 }
    }

    /// The tool declarations for the worker's harness.
    pub fn specs() -> Vec<HostToolSpec> {
        let spec = |name: &str, description: &str, schema: serde_json::Value| HostToolSpec {
            name: name.to_owned(),
            description: description.to_owned(),
            input_schema: schema.to_string(),
        };
        vec![
            spec(
                SANDBOX_BASH,
                "Runs a shell command inside the sandbox, in the workspace. Returns stdout, stderr and the exit code.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Script for /bin/sh -c (cmd /C on Windows)" },
                        "cwd": { "type": "string", "description": "Directory relative to the workspace" },
                        "timeout_ms": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }),
            ),
            spec(
                SANDBOX_READ,
                "Reads a file in the sandbox workspace.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "limit": { "type": "integer", "minimum": 0, "description": "Most bytes to read" }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                }),
            ),
            spec(
                SANDBOX_WRITE,
                "Writes a file in the sandbox workspace, creating parent directories.",
                serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" }, "content": { "type": "string" } },
                    "required": ["path", "content"],
                    "additionalProperties": false
                }),
            ),
            spec(
                SANDBOX_LIST,
                "Lists a directory in the sandbox workspace.",
                serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string", "description": "Defaults to the workspace root" } },
                    "additionalProperties": false
                }),
            ),
        ]
    }

    fn clip<'a>(&self, bytes: &'a [u8]) -> (std::borrow::Cow<'a, str>, bool) {
        let cut = bytes.len().min(self.output_limit);
        (String::from_utf8_lossy(&bytes[..cut]), cut < bytes.len())
    }

    async fn bash(&self, args: BashArgs) -> Result<String, String> {
        let exec = ExecRequest {
            command: Command::Shell(args.command),
            cwd: args.cwd,
            env: self.env.clone(),
            stdin: None,
            timeout_ms: args.timeout_ms,
        };
        let out = self.client.exec(exec).await.map_err(|e| e.to_string())?;
        let mut text = String::new();
        for (name, bytes) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
            if bytes.is_empty() {
                continue;
            }
            let (body, clipped) = self.clip(bytes);
            text.push_str(&format!("<{name}>\n{body}\n</{name}>\n"));
            if clipped {
                text.push_str(&format!("[{name} cut at {} of {} bytes]\n", self.output_limit, bytes.len()));
            }
        }
        match (out.status.code, out.status.signal) {
            _ if out.status.timed_out => text.push_str("killed: timed out"),
            (Some(code), _) => text.push_str(&format!("exit code: {code}")),
            (None, Some(signal)) => text.push_str(&format!("killed by signal {signal}")),
            (None, None) => text.push_str("exit status unknown"),
        }
        Ok(text)
    }

    async fn read(&self, args: ReadArgs) -> Result<String, String> {
        let data = self.client.read_file(&args.path, args.limit).await.map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&data).into_owned())
    }

    async fn write(&self, args: WriteArgs) -> Result<String, String> {
        let len = args.content.len();
        self.client.write_file(&args.path, args.content.into_bytes(), None).await.map_err(|e| e.to_string())?;
        Ok(format!("wrote {len} bytes to {}", args.path))
    }

    async fn list(&self, args: ListArgs) -> Result<String, String> {
        let path = args.path.as_deref().unwrap_or(".");
        let entries = self.client.list_dir(path).await.map_err(|e| e.to_string())?;
        let mut text = String::new();
        for entry in entries {
            let suffix = match entry.meta.kind {
                FileKind::Dir => "/",
                FileKind::Symlink => "@",
                FileKind::File | FileKind::Other => "",
            };
            text.push_str(&format!("{}{suffix}\t{}\n", entry.name, entry.meta.size));
        }
        Ok(text)
    }
}

#[async_trait]
impl HostTools for SandboxTools {
    async fn call(&self, _session_id: &str, tool: &str, args: &str) -> Result<String, String> {
        match tool {
            SANDBOX_BASH => self.bash(parse(args)?).await,
            SANDBOX_READ => self.read(parse(args)?).await,
            SANDBOX_WRITE => self.write(parse(args)?).await,
            SANDBOX_LIST => self.list(parse(args)?).await,
            other => Err(format!("unknown tool {other}")),
        }
    }
}

/// One worker run.
pub struct WorkerRequest {
    pub session_id: String,
    pub task: String,
    /// Instructions of the agent the worker plays, if one was asked for.
    pub agent: Option<String>,
    pub model_role: Option<String>,
    /// Host directory the harness runs in. Kept empty: the worker sees the
    /// workspace only through the sandbox.
    pub harness_cwd: PathBuf,
}

/// What the worker said at the end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerReport {
    pub session_id: String,
    pub final_message: Option<String>,
}

/// Instructions every worker gets ahead of its task.
const PREAMBLE: &str = "You are working inside a sandbox. Use sandbox_bash, sandbox_read, sandbox_write and \
sandbox_list for every command and file operation; paths are relative to the workspace root. End with a short \
report of what you did.";

/// Runs the worker session to the end of its first turn.
///
/// `template` carries the host side settings (node, model, harness env and
/// executable); the worker keeps them, gets the sandbox tools in place of
/// any host tools and MCP servers, and has the harness's own host tools
/// switched off.
pub async fn run_worker(
    adapter: &dyn Adapter,
    template: &SessionConfig,
    journal: &dyn WorkerJournal,
    tools: Arc<SandboxTools>,
    request: WorkerRequest,
) -> Result<WorkerReport> {
    let WorkerRequest { session_id, task, agent, model_role, harness_cwd } = request;
    let mut config = template.clone();
    config.session_id.clone_from(&session_id);
    config.cwd = harness_cwd;
    if model_role.is_some() {
        config.model_role = model_role;
    }
    config.context = Some(match (&template.context, agent) {
        (_, Some(agent)) => format!("{PREAMBLE}\n\n{agent}"),
        (Some(context), None) => format!("{PREAMBLE}\n\n{context}"),
        (None, None) => PREAMBLE.to_owned(),
    });
    config.mcp_servers.clear();
    config.host_tools = SandboxTools::specs();
    config.host_tool_handler = Some(tools);
    for tool in HOST_TOOLS_TO_DISABLE {
        if !config.disabled_tools.iter().any(|t| t == tool) {
            config.disabled_tools.push((*tool).to_owned());
        }
    }

    let (approvals, mut waiting) = tokio::sync::mpsc::unbounded_channel();
    let mirror = Arc::new(Mirror { inner: journal.open(&session_id)?, dom: Mutex::new(Dom::new()), approvals });
    let session = adapter.start(config, Arc::clone(&mirror) as Arc<dyn PatchSink>).await?;
    let by = template.node.clone();
    let run = async {
        session.send(Prompt { text: task, attachments: Vec::new(), by: by.clone() }).await?;
        session.wait_idle().await
    };
    // The mirror keeps the sender, so this only ends on a failed answer.
    let allow = async {
        while let Some(decision) = waiting.recv().await {
            let allow = Answer { values: vec![answer::ALLOW_ONCE.to_owned()], reason: None, by: by.clone() };
            session.answer(decision, allow).await?;
        }
        Ok(())
    };
    let result = tokio::select! {
        result = run => result,
        result = allow => result,
    };
    let shutdown = session.shutdown().await;
    result?;
    shutdown?;
    let final_message = final_message(&mirror.dom.lock().unwrap_or_else(PoisonError::into_inner));
    Ok(WorkerReport { session_id, final_message })
}

impl From<serde_json::Error> for SandboxError {
    fn from(e: serde_json::Error) -> Self {
        SandboxError::Args(e.to_string())
    }
}
