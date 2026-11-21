//! Gate checks a staged version must pass before it may be swapped in: the ports the
//! app implements (omp adapters live in loa-app) and the pure checks over their answers.

mod checks;

pub use checks::*;

use async_trait::async_trait;
use loa_settings::updates::{CheckResult, GateCheck, GateReport};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::RangeInclusive;
use std::path::Path;
use std::time::Duration;

/// Lists the tools an installed version offers.
#[async_trait]
pub trait ToolCatalog: Send + Sync {
    async fn tools(&self, executable: &Path) -> Result<Vec<ToolSchema>, String>;
}

/// Asks an installed version for its session format and config schema.
#[async_trait]
pub trait FormatProbe: Send + Sync {
    async fn session_version(&self, executable: &Path) -> Result<u32, String>;
    async fn config_keys(&self, executable: &Path) -> Result<ConfigSchema, String>;
}

/// Asks an installed version which RPC protocol versions it offers.
#[async_trait]
pub trait RpcProbe: Send + Sync {
    async fn protocol_versions(&self, executable: &Path) -> Result<Vec<u32>, String>;
}

/// Runs the adapter contract tests against an installed version.
#[async_trait]
pub trait ContractRunner: Send + Sync {
    async fn run(&self, executable: &Path) -> Result<Vec<ContractFailure>, String>;
}

/// Reviews changelog text for entries that break the app.
#[async_trait]
pub trait ChangelogSummarizer: Send + Sync {
    async fn review(&self, changelog: &str) -> Result<ChangelogReview, String>;
}

/// Every probe port of a fully gated product.
pub trait Probe: ToolCatalog + FormatProbe + RpcProbe + ContractRunner {}
impl<T: ToolCatalog + FormatProbe + RpcProbe + ContractRunner> Probe for T {}

/// Builds the gate adapters of one gate run; the app plugs in the omp-backed ones.
pub trait GatePorts: Send + Sync + fmt::Debug {
    fn probe(&self, gates: &OmpGates, timeout: Duration) -> Box<dyn Probe>;
    /// A reviewer running `model` on the `reviewer` executable.
    fn reviewer(&self, reviewer: &Path, model: &str, timeout: Duration) -> Box<dyn ChangelogSummarizer>;
}

/// What the app depends on in omp: used tools and fields, written config keys, migrations, RPC range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OmpGates {
    pub used_tools: UsedTools,
    pub written_keys: BTreeSet<String>,
    /// Session format migrations the adapter has, as `(from, to)` steps.
    pub migrations: BTreeSet<(u32, u32)>,
    pub rpc_supported: RangeInclusive<u32>,
}

/// Tools the app calls (bash command and todo for jobs, the bridged tools with omp's own schemas).
const USED_TOOLS: &[(&str, &[&str])] =
    &[("bash", &["command"]), ("todo", &[]), ("read", &[]), ("edit", &[]), ("ast_grep", &[]), ("ast_edit", &[]), ("lsp", &[]), ("debug", &[])];
/// Config keys the models screen and the adapter's overlay write.
const WRITTEN_KEYS: &[&str] =
    &["modelRoles", "retry.fallbackChains", "enabledModels", "enabledProviders", "disabledProviders", "modelProviderOrder", "memory.backend"];
const MIGRATIONS: &[(u32, u32)] = &[(1, 2), (2, 3)];

impl Default for OmpGates {
    fn default() -> Self {
        let used_tools = USED_TOOLS.iter().map(|(tool, fields)| (tool.to_string(), fields.iter().map(|f| f.to_string()).collect())).collect();
        let written_keys = WRITTEN_KEYS.iter().map(|k| k.to_string()).collect();
        Self { used_tools, written_keys, migrations: MIGRATIONS.iter().copied().collect(), rpc_supported: 1..=2 }
    }
}

/// Tools the app calls, each with the dotted argument fields it fills in.
pub type UsedTools = BTreeMap<String, BTreeSet<String>>;

/// Everything the gate checks need; `None` skips the checks that don't apply.
pub struct GateInputs<'a> {
    pub old_executable: &'a Path,
    pub new_executable: &'a Path,
    pub omp: Option<(&'a dyn Probe, &'a OmpGates)>,
    /// The reviewer and the release notes between the two versions.
    pub changelog: Option<(&'a dyn ChangelogSummarizer, &'a str)>,
}

/// Runs the applicable checks with their ports queried concurrently; a port error fails only its check.
pub async fn run_gates(product: &str, from: &Version, to: &Version, inputs: &GateInputs<'_>) -> GateReport {
    let (old, new) = (inputs.old_executable, inputs.new_executable);
    let tools = async {
        let (probe, gates) = inputs.omp?;
        let (o, n) = futures::join!(probe.tools(old), probe.tools(new));
        Some(pair(GateCheck::ToolSchemas, o, n, |o, n| check_tool_schemas(&o, &n, &gates.used_tools)))
    };
    let session = async {
        let (probe, gates) = inputs.omp?;
        let (o, n) = futures::join!(probe.session_version(old), probe.session_version(new));
        Some(pair(GateCheck::SessionFormat, o, n, |o, n| check_session_format(o, n, &gates.migrations)))
    };
    let config = async {
        let (probe, gates) = inputs.omp?;
        Some(pair(GateCheck::ConfigSchema, Ok(()), probe.config_keys(new).await, |(), s| check_config_schema(&gates.written_keys, &s)))
    };
    let rpc = async {
        let (probe, gates) = inputs.omp?;
        let offered = probe.protocol_versions(new).await;
        Some(pair(GateCheck::RpcProtocol, Ok(()), offered, |(), o| check_rpc_protocol(&o, gates.rpc_supported.clone())))
    };
    let contract = async {
        let (probe, _) = inputs.omp?;
        Some(pair(GateCheck::ContractTests, Ok(()), probe.run(new).await, |(), f| check_contract(&f)))
    };
    let changelog = async {
        let (reviewer, text) = inputs.changelog?;
        Some(reviewer.review(text).await.map_or_else(|e| CheckResult::failed_to_run(GateCheck::Changelog, e), |r| check_changelog(&r)))
    };
    let (tools, session, config, rpc, contract, changelog) = futures::join!(tools, session, config, rpc, contract, changelog);
    let checks = [tools, session, config, rpc, contract, changelog].into_iter().flatten().collect();
    GateReport { product: product.to_owned(), from: from.clone(), to: to.clone(), checks }
}

/// Runs `check` over both versions' answers, or fails naming the version whose port failed.
fn pair<A, B>(check: GateCheck, old: Result<A, String>, new: Result<B, String>, f: impl FnOnce(A, B) -> CheckResult) -> CheckResult {
    match (old, new) {
        (Ok(o), Ok(n)) => f(o, n),
        (Err(e), _) => CheckResult::failed_to_run(check, format!("current version: {e}")),
        (_, Err(e)) => CheckResult::failed_to_run(check, format!("staged version: {e}")),
    }
}
