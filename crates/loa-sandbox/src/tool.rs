//! The app-hosted `sandbox_task` tool and the worker/provider lifecycle.

use crate::client::StubClient;
use crate::isolation::IsolatedCheckout;
use crate::provider::{SandboxKind, SandboxOptions, SandboxProvider, SandboxSpec, WorkspaceMode};
use crate::sync;
use crate::worker::{SandboxTools, WorkerJournal, WorkerRequest, run_worker};
use crate::{Result, SandboxError};
use async_trait::async_trait;
use loa_adapter::{Adapter, HostToolSpec, HostTools, SessionConfig};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

/// Name registered with an adapter; it does not replace omp's own `task`.
pub const NAME: &str = "sandbox_task";

/// Run numbers, unique in this process, so containers, remote directories
/// and worker sessions of concurrent sessions never collide.
static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

/// One sandbox task's arguments.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxTaskArgs {
    pub task: String,
    pub kind: SandboxKind,
    #[serde(default)]
    pub options: SandboxOptions,
    #[serde(default)]
    pub isolation: bool,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model_role: Option<String>,
}

/// Isolated task results retain the worker's answer and a unified diff to apply deliberately.
#[derive(Debug, Serialize, Deserialize)]
pub struct SandboxTaskResult {
    pub session_id: String,
    pub final_message: Option<String>,
    /// A unified diff of the isolated checkout against its initial state; absent without isolation.
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

/// Host-owned tool service. The adapter, credentials and journal never enter `SandboxSpec`.
pub struct SandboxTask {
    adapter: Arc<dyn Adapter>,
    template: SessionConfig,
    journal: Arc<dyn WorkerJournal>,
    workspace: PathBuf,
    providers: BTreeMap<SandboxKind, Arc<dyn SandboxProvider>>,
    events: Option<broadcast::Sender<SandboxEvent>>,
}

impl SandboxTask {
    /// Creates the tool with a host adapter and host-side journal. Register providers before calling it.
    pub fn new(
        adapter: Arc<dyn Adapter>,
        template: SessionConfig,
        journal: Arc<dyn WorkerJournal>,
        workspace: PathBuf,
    ) -> Self {
        Self { adapter, template, journal, workspace, providers: BTreeMap::new(), events: None }
    }

    /// Installs one provider for its kind.
    pub fn register(&mut self, provider: Arc<dyn SandboxProvider>) {
        self.providers.insert(provider.kind(), provider);
    }

    /// Reports every run to `events`.
    pub fn with_events(mut self, events: broadcast::Sender<SandboxEvent>) -> Self {
        self.events = Some(events);
        self
    }

    /// The kinds with a registered provider.
    pub fn kinds(&self) -> Vec<SandboxKind> {
        self.providers.keys().copied().collect()
    }

    /// JSON Schema and description to give to each harness adapter. Only the
    /// registered kinds are offered, each described by its provider.
    pub fn spec(&self) -> HostToolSpec {
        let kinds: Vec<&str> = self.providers.keys().map(|kind| kind.as_str()).collect();
        let available = self.providers.iter()
            .map(|(kind, provider)| format!("- {}: {}", kind.as_str(), provider.describe()))
            .collect::<Vec<_>>()
            .join("\n");
        HostToolSpec {
            name: NAME.into(),
            description: format!(
                "Run a task as a worker agent whose commands and file operations happen inside a sandbox; its model \
                 calls stay on this host. Returns the worker's final message. With isolation (the workspace must be a \
                 git repository) the worker works on an isolated checkout and the result carries `patch`, a unified \
                 diff of its changes, while the workspace stays untouched. Without isolation the worker changes the \
                 workspace itself.\nSandboxes available here:\n{available}"
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "minLength": 1, "description": "Complete instructions for the worker"},
                    "kind": {"enum": kinds},
                    "options": {
                        "type": "object",
                        "properties": {
                            "image": {"type": "string"},
                            "host": {"type": "string"},
                            "peer": {"type": "string"},
                            "network": {"type": "boolean"},
                            "memory_mib": {"type": "integer", "minimum": 1},
                            "cpus": {"type": "integer", "minimum": 1}
                        },
                        "additionalProperties": false
                    },
                    "isolation": {"type": "boolean", "description": "Work on an isolated checkout and return a patch"},
                    "agent": {"type": "string", "description": "Instructions of the agent the worker plays"},
                    "model_role": {"type": "string"}
                },
                "required": ["task", "kind"],
                "additionalProperties": false
            }).to_string(),
        }
    }

    fn emit(&self, event: SandboxEvent) {
        if let Some(events) = &self.events {
            // No receiver only means nobody is watching.
            let _ = events.send(event);
        }
    }

    /// Runs the host worker and always stops its sandbox, including on worker errors.
    pub async fn execute(&self, args: SandboxTaskArgs) -> Result<SandboxTaskResult> {
        if args.task.trim().is_empty() {
            return Err(SandboxError::Args("task must not be empty".into()));
        }
        let provider = Arc::clone(self.providers.get(&args.kind).ok_or_else(|| SandboxError::NoProvider(args.kind.as_str().into()))?);
        let run = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
        // With the start time, ids stay unique across app restarts too, where
        // run numbers start over but earlier worker journals remain.
        let started = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis());
        let worker = format!("{}-sandbox-{started:x}-{run}", self.template.session_id);
        self.emit(SandboxEvent::Started(SandboxRun {
            run,
            parent: self.template.session_id.clone(),
            worker: worker.clone(),
            kind: args.kind,
            task: args.task.clone(),
            isolated: args.isolation,
            workspace: self.workspace.clone(),
        }));
        let result = self.run(provider.as_ref(), run, started, worker, args).await;
        match &result {
            Ok(result) => self.emit(SandboxEvent::Finished { run, final_message: result.final_message.clone(), patch: result.patch.clone() }),
            Err(error) => self.emit(SandboxEvent::Failed { run, error: error.to_string() }),
        }
        result
    }

    async fn run(&self, provider: &dyn SandboxProvider, run: u64, started: u128, worker: String, args: SandboxTaskArgs) -> Result<SandboxTaskResult> {
        let checkout = if args.isolation { Some(IsolatedCheckout::create(&self.workspace)?) } else { None };
        let workspace = checkout.as_ref().map_or(self.workspace.as_path(), |checkout| checkout.path());
        let mut sandbox = provider.start(&SandboxSpec {
            // Names containers and remote directories: unique across
            // processes and machines sharing a Docker daemon or SSH host.
            name: format!("loa-sbx-{}-{started:x}-{run}", std::process::id()),
            workspace: workspace.to_path_buf(),
            // Never inherit the host adapter's env (which can contain API keys).
            env: Vec::new(),
            options: args.options,
        }).await?;
        let result = async {
            let channel = sandbox.take_channel().ok_or(SandboxError::Disconnected)?;
            let client = Arc::new(StubClient::connect(channel).await?);
            if sandbox.workspace_mode() == WorkspaceMode::Copied {
                sync::copy_in(&client, workspace).await?;
            }
            // The harness needs a host cwd, not a sandbox mount. No host secrets or
            // checkout paths become worker-visible through this directory.
            let harness_dir = tempfile::tempdir()?;
            let report = run_worker(
                self.adapter.as_ref(),
                &self.template,
                self.journal.as_ref(),
                Arc::new(SandboxTools::new(Arc::clone(&client), Vec::new())),
                WorkerRequest {
                    session_id: worker,
                    task: args.task,
                    agent: args.agent,
                    model_role: args.model_role,
                    harness_cwd: harness_dir.path().to_path_buf(),
                },
            ).await?;
            if sandbox.workspace_mode() == WorkspaceMode::Copied {
                sync::copy_out(&client, workspace).await?;
            }
            client.shutdown().await?;
            let patch = checkout.as_ref().map(|checkout| checkout.patch()).transpose()?
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
            Ok(SandboxTaskResult { session_id: report.session_id, final_message: report.final_message, patch })
        }.await;
        let stopped = sandbox.stop().await;
        match (result, stopped) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(report), Ok(())) => Ok(report),
        }
    }
}

#[async_trait]
impl HostTools for SandboxTask {
    async fn call(&self, _session_id: &str, tool: &str, args: &str) -> std::result::Result<String, String> {
        if tool != NAME {
            return Err(format!("unknown tool `{tool}`"));
        }
        let args = serde_json::from_str(args).map_err(|error| format!("invalid sandbox_task arguments: {error}"))?;
        let result = self.execute(args).await.map_err(|error| error.to_string())?;
        serde_json::to_string(&result).map_err(|error| error.to_string())
    }
}
