//! Codex adapter: drives `codex app-server` (JSON-RPC over stdio) and reads and writes Codex rollout files.

pub mod mapper;
pub mod rollout;
mod session;

use crate::kit::NativeCodec;
use async_trait::async_trait;
use loa_contracts::harness::{
    Adapter, AdapterError, Availability, Capabilities, ContextDelivery, Feature, HarnessSession, ModelSwitch, Result,
    SessionConfig,
};
use loa_core::{DecisionKind, Dom, ElementKind, PatchSink, SCHEMA_VERSION, attr};
use mapper::Mapper;
use serde_json::Value as Json;
use session::{CodexSession, Open};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Harness id written to `<session harness>`.
pub const HARNESS: &str = "codex";

fn str_at<'a>(value: &'a Json, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn canonical(mut value: Json) -> String {
    value.sort_all_objects();
    value.to_string()
}

/// The Codex harness.
#[derive(Clone, Debug)]
pub struct CodexAdapter {
    executable: PathBuf,
    args: Vec<String>,
    /// This node's public key, written to imported sessions.
    node: String,
}

impl CodexAdapter {
    /// Runs `codex` from `PATH`; `node` is written to imported sessions.
    pub fn new(node: impl Into<String>) -> Self {
        Self { executable: PathBuf::from("codex"), args: Vec::new(), node: node.into() }
    }

    /// Runs `executable` instead of `codex` from `PATH`.
    pub fn with_executable(mut self, executable: impl Into<PathBuf>) -> Self {
        self.executable = executable.into();
        self
    }

    /// Extra global arguments passed before `app-server`.
    pub fn with_args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.args = args.into_iter().collect();
        self
    }
}

fn native_thread(dom: &Dom) -> Result<&str> {
    let thread = dom.root().and_then(|root| root.str_attr(attr::NATIVE_ID));
    thread.ok_or_else(|| AdapterError::Format("the session has no Codex thread id (`native_id`)".to_owned()))
}

#[async_trait]
impl Adapter for CodexAdapter {
    fn harness(&self) -> &'static str {
        HARNESS
    }

    async fn availability(&self) -> Availability {
        Availability::probe(&self.executable, &["--version"]).await
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: SCHEMA_VERSION,
            // thread/fork, thread/compact/start, collab agent threads, collaborationMode "plan".
            features: vec![Feature::Fork, Feature::Compaction, Feature::Subagents, Feature::PlanMode],
            model_switch: ModelSwitch::PerTurn,
            decisions: vec![DecisionKind::Approval, DecisionKind::Question, DecisionKind::Form],
            context_delivery: vec![ContextDelivery::SystemPrompt],
            // dynamicTools answered through item/tool/call; MCP servers as `-c mcp_servers.*` overrides.
            host_tools: true,
            mcp_servers: true,
            queue_mid_turn: false,
            effort: true,
            import: true,
            export: true,
        }
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let attach = config.clone();
        let session = CodexSession::open(self, config, sink, Open::Start, move |thread, model, w| {
            Mapper::start(&attach, thread, model, w)
        });
        Ok(Box::new(session.await?))
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let open = Open::Resume { thread_id: native_thread(dom)? };
        let session = CodexSession::open(self, config, sink, open, |_, _, w| Mapper::attach(dom, w));
        Ok(Box::new(session.await?))
    }

    async fn fork(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> Result<Box<dyn HarnessSession>> {
        let thread_id = native_thread(dom)?;
        let root = dom.root().map(|r| r.id).ok_or_else(|| AdapterError::Format("empty DOM".to_owned()))?;
        let last_turn_id = dom.children_of_kind(root, ElementKind::Turn).filter_map(|t| t.str_attr(attr::ID)).last();
        let session = CodexSession::open(self, config, sink, Open::Fork { thread_id, last_turn_id }, |thread, _, w| {
            w.set(root, attr::NATIVE_ID, thread)?;
            Mapper::attach(dom, w)
        });
        Ok(Box::new(session.await?))
    }

    fn import(&self, native: &Path, sink: &dyn PatchSink) -> Result<()> {
        rollout::import(BufReader::new(File::open(native)?), sink, &self.node)
    }

    fn export(&self, dom: &Dom, out: &Path) -> Result<()> {
        rollout::CodexNative::export(dom, out)
    }
}
