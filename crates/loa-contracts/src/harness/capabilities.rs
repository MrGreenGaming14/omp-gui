//! What a harness does natively; the app provides the rest on top.

use loa_core::DecisionKind;
use serde::{Deserialize, Serialize};

/// An omp feature the app guarantees on every harness.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Feature {
    Fork,
    /// Branch and rewind inside the harness's own session tree.
    Rewind,
    Compaction,
    ModelRoles,
    Subagents,
    /// The user can message a running subagent directly and read its live
    /// conversation ([`super::HarnessSession::message_subagent`]).
    SubagentMessages,
    PlanMode,
    /// Hashline edits, LSP, DAP, AST edits.
    OmpTools,
    AdvisorRules,
}

/// When the harness lets the model change.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ModelSwitch {
    #[default]
    Never,
    PerTurn,
    /// Any time, including mid turn.
    PerCall,
}

/// How project context reaches the harness.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ContextDelivery {
    SystemPrompt,
    ContextFile,
    McpResource,
}

/// Declared by `Adapter::capabilities`; `Default` only takes prompts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Element schema version the adapter emits.
    pub schema_version: u32,
    pub features: Vec<Feature>,
    pub model_switch: ModelSwitch,
    pub decisions: Vec<DecisionKind>,
    /// Best first.
    pub context_delivery: Vec<ContextDelivery>,
    pub host_tools: bool,
    pub mcp_servers: bool,
    /// A prompt sent mid turn is queued instead of `Busy`.
    pub queue_mid_turn: bool,
    pub effort: bool,
    pub import: bool,
    pub export: bool,
}

impl Capabilities {
    pub const fn builder() -> CapsBuilder {
        CapsBuilder(Capabilities {
            schema_version: 0, features: Vec::new(), model_switch: ModelSwitch::Never, decisions: Vec::new(),
            context_delivery: Vec::new(), host_tools: false, mcp_servers: false, queue_mid_turn: false,
            effort: false, import: false, export: false,
        })
    }

    pub fn has(&self, feature: Feature) -> bool { self.features.contains(&feature) }
}

/// Chained construction of [`Capabilities`].
#[derive(Clone, Debug)]
pub struct CapsBuilder(Capabilities);

macro_rules! setters {
    ($($flag:ident),* ; $($set:ident: $field:ident: $ty:ty),* ; $($add:ident: $list:ident: $item:ty),*) => {
        $(pub fn $flag(mut self) -> Self { self.0.$flag = true; self })*
        $(pub fn $set(mut self, v: $ty) -> Self { self.0.$field = v; self })*
        $(pub fn $add(mut self, v: &[$item]) -> Self { self.0.$list.extend_from_slice(v); self })*
    };
}

impl CapsBuilder {
    setters!(host_tools, mcp_servers, queue_mid_turn, effort, import, export;
        schema: schema_version: u32, model_switch: model_switch: ModelSwitch;
        features: features: Feature, decisions: decisions: DecisionKind, context: context_delivery: ContextDelivery);

    pub fn build(self) -> Capabilities { self.0 }
}
