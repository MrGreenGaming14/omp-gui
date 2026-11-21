//! The app-hosted `sandbox_task` tool and the worker/provider lifecycle.

use crate::client::StubClient;
use crate::isolation::IsolatedCheckout;
use crate::provider::{SandboxKind, SandboxOptions, SandboxProvider, SandboxSpec, WorkspaceMode};
use crate::worker::{WorkerJournal, run_worker, sandbox_tools};
use crate::{Result, SandboxError, sync};
use async_trait::async_trait;
use loa_contracts::harness::{Adapter, SessionConfig};
use loa_contracts::tools::{CallCtx, Tool, ToolOutput};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::broadcast;

/// Name registered with an adapter; it does not replace omp's own `task`.
pub const NAME: &str = "sandbox_task";

/// Run numbers, unique in this process, so containers, remote directories
/// and worker sessions of concurrent sessions never collide.
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

/// One sandbox task's arguments.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SandboxTaskArgs {
    /// Complete instructions for the worker
    pub task: String,
    pub kind: SandboxKind,
    #[serde(default)]
    pub options: SandboxOptions,
    /// Work on an isolated checkout and return a patch
    #[serde(default)]
    pub isolation: bool,
    /// Instructions of the agent the worker plays
    pub agent: Option<String>,
    pub model_role: Option<String>,
}

/// The worker's answer and, for an isolated run, a unified diff of its
/// checkout against its initial state, to apply deliberately.
#[derive(Debug, Serialize, Deserialize)]
pub struct SandboxTaskResult {
    pub session_id: String,
    pub final_message: Option<String>,
    pub patch: Option<String>,
}

/// A `sandbox_task` call as it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxRun {
    pub run: u64,
    /// Session that called the tool.
    pub parent: String,
    /// The worker's session id.
    pub worker: String,
    pub kind: SandboxKind,
    pub task: String,
    pub isolated: bool,
    /// The workspace the task works on (the original, not the checkout).
    pub workspace: PathBuf,
}

/// What happens to `sandbox_task` calls, for the app to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SandboxEvent {
    Started(SandboxRun),
    Finished { run: u64, final_message: Option<String>, patch: Option<String> },
    Failed { run: u64, error: String },
}

/// The host-owned tool. The adapter, credentials and journal never enter a [`SandboxSpec`].
pub struct SandboxTask {
    adapter: Arc<dyn Adapter>,
    template: SessionConfig,
    journal: Arc<dyn WorkerJournal>,
    providers: BTreeMap<SandboxKind, Arc<dyn SandboxProvider>>,
    events: broadcast::Sender<SandboxEvent>,
    description: String,
}

impl SandboxTask {
    /// The tool for sessions like `template`, whose runs go to `events`.
    pub fn new(adapter: Arc<dyn Adapter>, template: SessionConfig, journal: Arc<dyn WorkerJournal>, providers: Vec<Arc<dyn SandboxProvider>>, events: broadcast::Sender<SandboxEvent>) -> Self {
        let providers: BTreeMap<_, _> = providers.into_iter().map(|provider| (provider.kind(), provider)).collect();
        let available: Vec<String> = providers.iter().map(|(kind, provider)| format!("- {kind}: {}", provider.describe())).collect();
        let description = format!(
            "Run a task as a worker agent whose commands and file operations happen inside a sandbox; its model \
             calls stay on this host. Returns the worker's final message. With isolation (the workspace must be a \
             git repository) the worker works on an isolated checkout and the result carries `patch`, a unified \
             diff of its changes, while the workspace stays untouched. Without isolation the worker changes the \
             workspace itself.\nSandboxes available here:\n{}",
            available.join("\n")
        );
        Self { adapter, template, journal, providers, events, description }
    }

    /// The kinds with a provider.
    pub fn kinds(&self) -> Vec<SandboxKind> {
        self.providers.keys().copied().collect()
    }

    fn emit(&self, event: SandboxEvent) {
        // No receiver only means nobody is watching.
        let _ = self.events.send(event);
    }

    /// Runs the host worker and always stops its sandbox, including on worker errors.
    pub async fn execute(&self, args: SandboxTaskArgs) -> Result<SandboxTaskResult> {
        if args.task.trim().is_empty() {
            return Err(SandboxError::Args("task must not be empty".into()));
        }
        let provider = Arc::clone(self.providers.get(&args.kind).ok_or_else(|| SandboxError::NoProvider(args.kind.to_string()))?);
        let run = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
        // With the start time, ids stay unique across app restarts too, where
        // run numbers start over but earlier worker journals remain.
        let started = loa_util::time::now_ms();
        let worker = format!("{}-sandbox-{started:x}-{run}", self.template.session_id);
        let (parent, workspace) = (self.template.session_id.clone(), self.template.cwd.clone());
        let (kind, task, isolated) = (args.kind, args.task.clone(), args.isolation);
        self.emit(SandboxEvent::Started(SandboxRun { run, parent, worker: worker.clone(), kind, task, isolated, workspace }));
        // Names containers and remote directories: unique across processes
        // and machines sharing a Docker daemon or SSH host.
        let name = format!("loa-sbx-{}-{started:x}-{run}", std::process::id());
        let result = self.work(provider.as_ref(), name, worker, args).await;
        self.emit(match &result {
            Ok(result) => SandboxEvent::Finished { run, final_message: result.final_message.clone(), patch: result.patch.clone() },
            Err(error) => SandboxEvent::Failed { run, error: error.to_string() },
        });
        result
    }

    async fn work(&self, provider: &dyn SandboxProvider, name: String, worker: String, args: SandboxTaskArgs) -> Result<SandboxTaskResult> {
        let checkout = if args.isolation { Some(IsolatedCheckout::create(&self.template.cwd)?) } else { None };
        let workspace = checkout.as_ref().map_or(self.template.cwd.as_path(), |checkout| checkout.path());
        // Never inherit the host adapter's env (which can contain API keys).
        let spec = SandboxSpec { name, workspace: workspace.to_path_buf(), env: Vec::new(), options: args.options.clone() };
        let mut sandbox = provider.start(&spec).await?;
        let copied = sandbox.workspace_mode() == WorkspaceMode::Copied;
        let result = async {
            let client = Arc::new(StubClient::connect(sandbox.take_channel().ok_or(SandboxError::Disconnected)?).await?);
            if copied {
                sync::copy_in(&client, workspace).await?;
            }
            // The harness needs a host cwd that shows nothing of the host or the checkout.
            let harness_dir = tempfile::tempdir()?;
            let final_message = run_worker(self.adapter.as_ref(), &self.template, self.journal.as_ref(), sandbox_tools(&client), &worker, harness_dir.path(), args).await?;
            if copied {
                sync::copy_out(&client, workspace).await?;
            }
            client.shutdown().await?;
            let patch = checkout.as_ref().map(IsolatedCheckout::patch).transpose()?.map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
            Ok::<_, SandboxError>(SandboxTaskResult { session_id: worker, final_message, patch })
        }
        .await;
        let stopped = sandbox.stop().await;
        let result = result?;
        stopped.map(|()| result)
    }
}

#[async_trait]
impl Tool for SandboxTask {
    type Args = SandboxTaskArgs;
    const NAME: &'static str = NAME;
    fn description(&self) -> &str { &self.description }
    async fn run(&self, _: &CallCtx, args: SandboxTaskArgs) -> Result<ToolOutput, String> {
        let result = self.execute(args).await.map_err(|error| error.to_string())?;
        serde_json::to_string(&result).map(ToolOutput::text).map_err(|error| error.to_string())
    }
}
