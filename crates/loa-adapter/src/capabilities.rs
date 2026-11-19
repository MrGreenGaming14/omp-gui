//! What a harness does natively.

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
    PlanMode,
    /// Hashline edits, LSP, DAP, AST edits.
    OmpTools,
    AdvisorRules,
}

impl Feature {
    pub const ALL: &'static [Feature] = &[
        Feature::Fork,
        Feature::Rewind,
        Feature::Compaction,
        Feature::ModelRoles,
        Feature::Subagents,
        Feature::PlanMode,
        Feature::OmpTools,
        Feature::AdvisorRules,
    ];
}

/// When the harness lets the model change.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ModelSwitch {
    /// Fixed for the life of the session.
    #[default]
    Never,
    /// Between turns.
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

/// Declared by [`Adapter::capabilities`](crate::Adapter::capabilities).
/// `Default` is a harness that can only take prompts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Element schema version the adapter emits.
    pub schema_version: u32,
    /// Features the harness has natively.
    pub features: Vec<Feature>,
    pub model_switch: ModelSwitch,
    /// Decision kinds the harness can raise.
    pub decisions: Vec<DecisionKind>,
    /// Ways to deliver project context, best first.
    pub context_delivery: Vec<ContextDelivery>,
    /// Accepts tools served by the app.
    pub host_tools: bool,
    /// Connects to MCP servers passed in the session config.
    pub mcp_servers: bool,
    /// A prompt sent mid turn is queued by the harness instead of `Busy`.
    pub queue_mid_turn: bool,
    /// Supports a reasoning effort setting.
    pub effort: bool,
    pub import: bool,
    pub export: bool,
}

impl Capabilities {
    pub fn has(&self, feature: Feature) -> bool {
        self.features.contains(&feature)
    }

    /// Features the app has to provide on top for this harness.
    pub fn missing(&self) -> impl Iterator<Item = Feature> {
        Feature::ALL.iter().copied().filter(|f| !self.has(*f))
    }
}
