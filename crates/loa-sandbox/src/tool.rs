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

/// Name registered with an adapter; it does not replace omp's own `task`.
pub const NAME: &str = "sandbox_task";

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

/// Host-owned tool service. The adapter, credentials and journal never enter `SandboxSpec`.
pub struct SandboxTask {
    adapter: Arc<dyn Adapter>,
    template: SessionConfig,
    journal: Arc<dyn WorkerJournal>,
    workspace: PathBuf,
    providers: BTreeMap<SandboxKind, Arc<dyn SandboxProvider>>,
    next_id: AtomicU64,
}

impl SandboxTask {
    /// Creates the tool with a host adapter and host-side journal. Register providers before calling it.
    pub fn new(
        adapter: Arc<dyn Adapter>,
        template: SessionConfig,
        journal: Arc<dyn WorkerJournal>,
        workspace: PathBuf,
    ) -> Self {
        Self { adapter, template, journal, workspace, providers: BTreeMap::new(), next_id: AtomicU64::new(1) }
    }

    /// Installs one provider for its kind.
    pub fn register(&mut self, provider: Arc<dyn SandboxProvider>) {
        self.providers.insert(provider.kind(), provider);
    }

    /// JSON Schema and description to give to each harness adapter.
    pub fn spec() -> HostToolSpec {
        HostToolSpec {
            name: NAME.into(),
            description: "Run a worker on the host, routing its command and file tools to a sandbox. With isolation, return its final message and a patch without changing the original workspace.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {"type": "string", "minLength": 1},
                    "kind": {"enum": ["docker", "firecracker", "vm", "ssh", "peer", "local"]},
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
                    "isolation": {"type": "boolean"},
                    "agent": {"type": "string"},
                    "model_role": {"type": "string"}
                },
                "required": ["task", "kind"],
                "additionalProperties": false
            }).to_string(),
        }
    }

    /// Runs the host worker and always stops its sandbox, including on worker errors.
    pub async fn execute(&self, args: SandboxTaskArgs) -> Result<SandboxTaskResult> {
        if args.task.trim().is_empty() {
            return Err(SandboxError::Args("task must not be empty".into()));
        }
        match args.kind {
            SandboxKind::Docker if args.options.image.as_deref().is_none_or(str::is_empty) => {
                return Err(SandboxError::Args("docker requires options.image".into()));
            }
            SandboxKind::Ssh if args.options.host.as_deref().is_none_or(str::is_empty) => {
                return Err(SandboxError::Args("ssh requires options.host".into()));
            }
            SandboxKind::Peer if args.options.peer.as_deref().is_none_or(str::is_empty) => {
                return Err(SandboxError::Args("peer requires options.peer".into()));
            }
            _ => {}
        }
        let provider = self.providers.get(&args.kind).ok_or_else(|| SandboxError::NoProvider(args.kind.as_str().into()))?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let name = format!("sandbox-{id}");
        let checkout = if args.isolation { Some(IsolatedCheckout::create(&self.workspace)?) } else { None };
        let workspace = checkout.as_ref().map_or(self.workspace.as_path(), |checkout| checkout.path());
        let mut sandbox = provider.start(&SandboxSpec {
            name: name.clone(),
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
                    session_id: format!("{}-{name}", self.template.session_id),
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
