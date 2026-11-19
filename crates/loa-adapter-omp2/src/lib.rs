//! omp2 adapter mapping patch@1 onto the app DOM.

mod map;
mod session;
pub mod translate;
pub mod wire;

use async_trait::async_trait;
use loa_adapter::{Adapter, AdapterError, Capabilities, Feature, HarnessSession, ModelSwitch, Result, SessionConfig};
use loa_core::{Dom, Draft, ElementKind, NodeId, PatchSink, SCHEMA_VERSION, Value, attr, now_ms};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;

pub use session::Omp2Session;

/// Stable harness identifier for the Rust omp2 branch.
pub const HARNESS: &str = "omp2";

/// Starts the omp2 executable and translates its DOM events into app patches.
#[derive(Default)]
pub struct Omp2Adapter;

impl Omp2Adapter {
    /// Constructs an adapter; the executable comes from `SessionConfig`.
    pub fn new() -> Self {
        Self
    }

    fn command(config: &SessionConfig, resume: Option<&str>) -> Result<Command> {
        if !config.mcp_servers.is_empty() || !config.host_tools.is_empty() || !config.disabled_tools.is_empty() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "configuring tools over omp2 RPC" });
        }
        if config.context.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "injecting project context over omp2 RPC" });
        }
        if config.model_role.is_some() || config.mode.is_some() {
            return Err(AdapterError::Unsupported { harness: HARNESS, what: "model roles or mode over omp2 RPC" });
        }
        let executable = config.executable.as_deref().unwrap_or(std::path::Path::new("omp"));
        let mut command = Command::new(executable);
        command.args(wire::RPC_ARGS).current_dir(&config.cwd)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        if let Some(native) = resume {
            command.arg(wire::RESUME_FLAG).arg(native);
        }
        if let Some(model) = &config.model {
            command.arg(wire::MODEL_FLAG).arg(model);
        }
        if let Some(effort) = &config.effort {
            command.arg(wire::THINKING_FLAG).arg(effort);
        }
        command.envs(&config.env);
        Ok(command)
    }

    async fn launch(config: &SessionConfig, sink: Arc<dyn PatchSink>, translator: translate::Translator, resume: Option<&str>) -> Result<(Omp2Session, Option<String>)> {
        let mut command = Self::command(config, resume)?;
        let child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                AdapterError::NotInstalled(config.executable.as_deref().unwrap_or(std::path::Path::new("omp")).display().to_string())
            } else {
                AdapterError::Io(error)
            }
        })?;
        Omp2Session::launch(child, sink, translator).await
    }
}

#[async_trait]
impl Adapter for Omp2Adapter {
    fn harness(&self) -> &'static str {
        HARNESS
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: SCHEMA_VERSION,
            features: vec![Feature::Compaction, Feature::Subagents, Feature::OmpTools, Feature::AdvisorRules],
            model_switch: ModelSwitch::PerTurn,
            decisions: vec![loa_core::DecisionKind::Approval, loa_core::DecisionKind::Question],
            effort: true,
            ..Capabilities::default()
        }
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        // Validate the command before mutating the app DOM.
        let _ = Self::command(&config, None)?;
        let ts = now_ms();
        let mut attrs = loa_core::Attrs::from([
            (attr::ID.to_owned(), Value::Str(config.session_id.clone())),
            (attr::HARNESS.to_owned(), Value::from(HARNESS)),
            (attr::NODE.to_owned(), Value::Str(config.node.clone())),
            (attr::CREATED.to_owned(), Value::Int(ts)),
            (attr::SCHEMA.to_owned(), Value::Int(i64::from(SCHEMA_VERSION))),
            (attr::CWD.to_owned(), Value::Str(config.cwd.to_string_lossy().into_owned())),
        ]);
        for (key, value) in [(attr::PROJECT, &config.project), (attr::MODEL, &config.model), (attr::EFFORT, &config.effort)] {
            if let Some(value) = value {
                attrs.insert(key.to_owned(), Value::Str(value.clone()));
            }
        }
        let root = sink.commit_at(Draft::Insert {
            parent: NodeId::DOCUMENT, index: None, kind: ElementKind::Session, attrs, text: String::new(),
        }, ts)?.node;
        let (session, native) = Self::launch(&config, sink.clone(), translate::Translator::new(root), None).await?;
        if let Some(native) = native {
            sink.commit(Draft::set(root, attr::NATIVE_ID, native))?;
        }
        Ok(Box::new(session))
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let root = dom.root().ok_or_else(|| AdapterError::Format("missing session root".into()))?;
        let native = root.str_attr(attr::NATIVE_ID).ok_or_else(|| AdapterError::Format("missing omp2 native session id".into()))?;
        let translator = translate::Translator::from_dom(dom).map_err(|error| AdapterError::Format(error.to_string()))?;
        let (session, _) = Self::launch(&config, sink, translator, Some(native)).await?;
        Ok(Box::new(session))
    }
}
