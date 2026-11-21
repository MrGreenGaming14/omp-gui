//! The worker session of a `sandbox_task` and the tools it works with.
//!
//! The worker runs on the host through a harness [`Adapter`], so its model
//! calls, API keys and journal never leave the host. Its harness gets no file
//! or exec tools of its own and no MCP servers; the only way it can act is
//! through the `sandbox_*` tools here, each of which is one request to the
//! stub inside the sandbox.

use crate::Result;
use crate::client::StubClient;
use crate::proto::ExecRequest;
use async_trait::async_trait;
use loa_contracts::harness::{Adapter, Answer, Prompt, SessionConfig};
use loa_contracts::tools::{CallCtx, Tool, ToolOutput, ToolSet};
use loa_core::{CommitError, DecisionKind, DecisionStatus, Dom, Draft, ElementKind, MessageKind, NodeId, Op, Patch, PatchSink, Role, answer, attr};
use loa_util::LockExt;
use schemars::JsonSchema;
use serde::Deserialize;
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Tools of the harness itself that act on the host. They are switched off
/// for a worker so everything it does goes through the sandbox.
pub const HOST_TOOLS_TO_DISABLE: &[&str] = &[
    "bash", "read", "write", "edit", "grep", "find", "glob", "ast_grep", "ast_edit", "eval", "lsp", "debug", "browser",
    "computer", "task", "fetch", "notebook", "ssh", "ask",
];

/// Most bytes of output returned to the model per stream.
const OUTPUT_LIMIT: usize = 64 * 1024;

/// Where a worker's session is journaled. `loa-app` backs it with the
/// session library, so the worker's journal lives on the host like any other.
pub trait WorkerJournal: Send + Sync {
    /// The sink of a new, empty session.
    fn open(&self, session_id: &str) -> Result<Arc<dyn PatchSink>>;
}

/// Forwards commits to the journal sink and keeps a copy of the DOM, so the
/// worker's final message can be read whatever the journal is. Approval
/// requests the worker's harness raises go to `approvals`: nobody watches a
/// worker, and its tools only reach the sandbox the user already approved
/// with the `sandbox_task` call.
struct Mirror {
    inner: Arc<dyn PatchSink>,
    dom: Mutex<Dom>,
    approvals: tokio::sync::mpsc::UnboundedSender<NodeId>,
}

impl PatchSink for Mirror {
    fn commit_at(&self, draft: Draft, ts: i64) -> Result<Patch, CommitError> {
        // Held across the commit so patches reach the mirror in sequence.
        let mut dom = self.dom.locked();
        let patch = self.inner.commit_at(draft, ts)?;
        dom.apply(&patch)?;
        let touches_status = matches!(&patch.op, Op::Insert { kind: ElementKind::Decision, .. })
            || matches!(&patch.op, Op::Set { name, .. } if name == attr::STATUS);
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
    let assistant = |n: &&loa_core::Node| {
        n.kind == ElementKind::Message
            && n.str_attr(attr::ROLE) == Some(Role::Assistant.as_str())
            && n.str_attr(attr::KIND) != Some(MessageKind::Thinking.as_str())
    };
    dom.iter().filter(assistant).last().map(|n| n.text.clone())
}

/// `sandbox_bash`: one shell command in the stub.
pub struct SandboxBash(pub Arc<StubClient>);
/// `sandbox_read`
pub struct SandboxRead(pub Arc<StubClient>);
/// `sandbox_write`
pub struct SandboxWrite(pub Arc<StubClient>);
/// `sandbox_list`
pub struct SandboxList(pub Arc<StubClient>);

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BashArgs {
    /// Script for /bin/sh -c
    pub command: String,
    /// Directory relative to the workspace
    pub cwd: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    pub path: String,
    /// Most bytes to read
    pub limit: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteArgs {
    pub path: String,
    pub content: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    /// Defaults to the workspace root
    pub path: Option<String>,
}

fn failed(e: crate::SandboxError) -> String { e.to_string() }

#[async_trait]
impl Tool for SandboxBash {
    type Args = BashArgs;
    const NAME: &'static str = "sandbox_bash";
    fn description(&self) -> &str { "Runs a shell command inside the sandbox, in the workspace. Returns stdout, stderr and the exit code." }
    async fn run(&self, _: &CallCtx, args: BashArgs) -> Result<ToolOutput, String> {
        let exec = ExecRequest { cwd: args.cwd, timeout_ms: args.timeout_ms, ..ExecRequest::shell(&args.command, &[]) };
        let out = self.0.exec(exec).await.map_err(failed)?;
        let mut text = String::new();
        for (name, bytes) in [("stdout", &out.stdout), ("stderr", &out.stderr)].into_iter().filter(|(_, b)| !b.is_empty()) {
            let cut = bytes.len().min(OUTPUT_LIMIT);
            let _ = writeln!(text, "<{name}>\n{}\n</{name}>", String::from_utf8_lossy(&bytes[..cut]));
            if cut < bytes.len() {
                let _ = writeln!(text, "[{name} cut at {OUTPUT_LIMIT} of {} bytes]", bytes.len());
            }
        }
        let _ = match (out.status.code, out.status.signal) {
            _ if out.status.timed_out => write!(text, "killed: timed out"),
            (Some(code), _) => write!(text, "exit code: {code}"),
            (None, Some(signal)) => write!(text, "killed by signal {signal}"),
            (None, None) => write!(text, "exit status unknown"),
        };
        Ok(ToolOutput::text(text))
    }
}

#[async_trait]
impl Tool for SandboxRead {
    type Args = ReadArgs;
    const NAME: &'static str = "sandbox_read";
    fn description(&self) -> &str { "Reads a file in the sandbox workspace." }
    async fn run(&self, _: &CallCtx, args: ReadArgs) -> Result<ToolOutput, String> {
        let script = args.limit.map_or_else(|| "cat -- \"$1\"".to_owned(), |limit| format!("head -c {limit} -- \"$1\""));
        let data = self.0.run(ExecRequest::shell(&script, &[&args.path])).await.map_err(failed)?;
        Ok(ToolOutput::text(String::from_utf8_lossy(&data)))
    }
}

#[async_trait]
impl Tool for SandboxWrite {
    type Args = WriteArgs;
    const NAME: &'static str = "sandbox_write";
    fn description(&self) -> &str { "Writes a file in the sandbox workspace, creating parent directories." }
    async fn run(&self, _: &CallCtx, args: WriteArgs) -> Result<ToolOutput, String> {
        let len = args.content.len();
        let script = ExecRequest::shell(r#"mkdir -p -- "$(dirname -- "$1")" && cat > "$1""#, &[&args.path]);
        self.0.run(ExecRequest { stdin: Some(args.content.into_bytes()), ..script }).await.map_err(failed)?;
        Ok(ToolOutput::text(format!("wrote {len} bytes to {}", args.path)))
    }
}

#[async_trait]
impl Tool for SandboxList {
    type Args = ListArgs;
    const NAME: &'static str = "sandbox_list";
    fn description(&self) -> &str { "Lists a directory in the sandbox workspace." }
    async fn run(&self, _: &CallCtx, args: ListArgs) -> Result<ToolOutput, String> {
        let listing = self.0.run(ExecRequest::shell(r#"ls -Al -- "$1""#, &[args.path.as_deref().unwrap_or(".")])).await.map_err(failed)?;
        Ok(ToolOutput::text(String::from_utf8_lossy(&listing)))
    }
}

/// The `sandbox_*` tools, each answered by the stub behind `client`.
pub fn sandbox_tools(client: &Arc<StubClient>) -> ToolSet {
    let mut tools = ToolSet::new();
    tools.register(SandboxBash(Arc::clone(client))).register(SandboxRead(Arc::clone(client)));
    tools.register(SandboxWrite(Arc::clone(client))).register(SandboxList(Arc::clone(client)));
    tools
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
/// executable); the worker keeps them, gets `tools` in place of any host
/// tools and MCP servers, and has the harness's own host tools switched off.
pub async fn run_worker(adapter: &dyn Adapter, template: &SessionConfig, journal: &dyn WorkerJournal, tools: ToolSet, request: WorkerRequest) -> Result<WorkerReport> {
    let WorkerRequest { session_id, task, agent, model_role, harness_cwd } = request;
    let mut config = template.clone();
    config.session_id.clone_from(&session_id);
    config.cwd = harness_cwd;
    config.model_role = model_role.or(config.model_role);
    config.context = Some(match agent.or(template.context.clone()) {
        Some(extra) => format!("{PREAMBLE}\n\n{extra}"),
        None => PREAMBLE.to_owned(),
    });
    config.mcp_servers.clear();
    config.host_tools = tools.specs();
    config.host_tool_handler = Some(Arc::new(tools));
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
            session.answer(decision, Answer { values: vec![answer::ALLOW_ONCE.to_owned()], reason: None, by: by.clone() }).await?;
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
    let final_message = final_message(&mirror.dom.locked());
    Ok(WorkerReport { session_id, final_message })
}
