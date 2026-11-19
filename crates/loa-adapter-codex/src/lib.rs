//! Codex adapter.
//!
//! Drives the OpenAI Codex CLI through `codex app-server` (JSON-RPC over
//! stdio): [`mapper`] turns its notifications and approval requests into DOM
//! patches, [`session`] runs the process. [`rollout`] imports and exports
//! Codex's own session files, `~/.codex/sessions/**/rollout-*.jsonl`.

mod json;
pub mod mapper;
pub mod rollout;
pub mod session;

use async_trait::async_trait;
use loa_adapter::{
    Adapter, AdapterError, Capabilities, ContextDelivery, Feature, HarnessSession, ModelSwitch, Result, SessionConfig,
};
use loa_core::{DecisionKind, Dom, Draft, ElementKind, PatchSink, SCHEMA_VERSION, attr};
use mapper::Mapper;
use session::{CodexSession, Open};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Harness id written to `<session harness>`.
pub const HARNESS: &str = "codex";

/// The Codex harness.
#[derive(Clone, Debug)]
pub struct CodexAdapter {
    executable: PathBuf,
    /// Arguments placed before the `app-server` subcommand.
    args: Vec<String>,
    /// Owner node key written to `<session node>` on import.
    node: String,
}

impl CodexAdapter {
    /// Runs `codex` from `PATH`. `node` is this node's public key, written to
    /// imported sessions.
    pub fn new(node: impl Into<String>) -> Self {
        Self { executable: PathBuf::from("codex"), args: Vec::new(), node: node.into() }
    }

    /// Runs `executable` instead of `codex` from `PATH`.
    pub fn with_executable(mut self, executable: impl Into<PathBuf>) -> Self {
        self.executable = executable.into();
        self
    }

    /// Extra global arguments, such as `-c key=value` overrides, passed before `app-server`.
    pub fn with_args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.args = args.into_iter().collect();
        self
    }
}

/// Codex's home directory from the environment: `$CODEX_HOME`, else `$HOME/.codex`.
pub fn codex_home_from_env() -> Option<PathBuf> {
    match std::env::var_os("CODEX_HOME") {
        Some(home) if !home.is_empty() => Some(PathBuf::from(home)),
        _ => std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")),
    }
}

/// Every `rollout-*.jsonl` under `codex_home/sessions`, sorted by path (which
/// sorts by date).
pub fn rollout_files(codex_home: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut stack = vec![codex_home.join("sessions")];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

fn native_thread(dom: &Dom) -> Result<String> {
    dom.root()
        .and_then(|root| root.str_attr(attr::NATIVE_ID))
        .map(str::to_owned)
        .ok_or_else(|| AdapterError::Format("the session has no Codex thread id (`native_id`)".to_owned()))
}

#[async_trait]
impl Adapter for CodexAdapter {
    fn harness(&self) -> &'static str {
        HARNESS
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: SCHEMA_VERSION,
            // thread/fork, thread/compact/start, collab agent threads,
            // collaborationMode "plan".
            features: vec![Feature::Fork, Feature::Compaction, Feature::Subagents, Feature::PlanMode],
            // turn/start takes a model override that sticks for later turns.
            model_switch: ModelSwitch::PerTurn,
            decisions: vec![DecisionKind::Approval, DecisionKind::Question, DecisionKind::Form],
            // thread/start developerInstructions.
            context_delivery: vec![ContextDelivery::SystemPrompt],
            // thread/start dynamicTools answered through item/tool/call.
            host_tools: true,
            // `-c mcp_servers.<name>.*` overrides.
            mcp_servers: true,
            queue_mid_turn: false,
            effort: true,
            import: true,
            export: true,
        }
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let attach_config = config.clone();
        let session = CodexSession::open(self, config, Open::Start, move |thread, model| {
            Mapper::start(&attach_config, thread, model, sink)
        })
        .await?;
        Ok(Box::new(session))
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let thread_id = native_thread(dom)?;
        let session = CodexSession::open(self, config, Open::Resume { thread_id: &thread_id }, |_, _| {
            Mapper::attach(dom, sink)
        })
        .await?;
        Ok(Box::new(session))
    }

    async fn fork(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let thread_id = native_thread(dom)?;
        let root = dom.root().map(|r| r.id).ok_or_else(|| AdapterError::Format("empty DOM".to_owned()))?;
        let last_turn = dom.children_of_kind(root, ElementKind::Turn).filter_map(|t| t.str_attr(attr::ID)).last();
        let open = Open::Fork { thread_id: &thread_id, last_turn_id: last_turn };
        let session = CodexSession::open(self, config, open, |new_thread, _| {
            sink.commit(Draft::set(root, attr::NATIVE_ID, new_thread))?;
            Mapper::attach(dom, sink)
        })
        .await?;
        Ok(Box::new(session))
    }

    fn import(&self, native: &Path, sink: &dyn PatchSink) -> Result<()> {
        let file = File::open(native)?;
        rollout::import(BufReader::new(file), sink, &self.node)
    }

    fn export(&self, dom: &Dom, out: &Path) -> Result<()> {
        let mut writer = BufWriter::new(File::create(out)?);
        rollout::export(dom, &mut writer)?;
        writer.flush()?;
        Ok(())
    }
}
