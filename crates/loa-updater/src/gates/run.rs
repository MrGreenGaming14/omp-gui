//! Runs every applicable gate check for one staged update.

use super::changelog::{ChangelogSummarizer, check_changelog};
use super::contract::{ContractRunner, check_contract};
use super::formats::{ConfigSchema, FormatProbe, check_config_schema, check_session_format};
use super::rpc::{RpcProbe, check_rpc_protocol};
use super::tools::{ToolCatalog, UsedTools, check_tool_schemas};
use super::{CheckResult, GateCheck, GateReport};
use semver::Version;
use std::collections::BTreeSet;
use std::ops::RangeInclusive;
use std::path::Path;

/// Inputs for the tool schema check.
pub struct ToolGate<'a> {
    pub catalog: &'a dyn ToolCatalog,
    pub used: &'a UsedTools,
}

/// Inputs for the session format and config schema checks.
pub struct FormatGate<'a> {
    pub probe: &'a dyn FormatProbe,
    /// Session format migrations the adapter has, as `(from, to)` steps.
    pub migrations: &'a BTreeSet<(u32, u32)>,
    /// Dotted config keys the app writes.
    pub written_keys: &'a BTreeSet<String>,
}

/// Inputs for the RPC protocol check.
pub struct RpcGate<'a> {
    pub probe: &'a dyn RpcProbe,
    /// Protocol versions the adapter speaks.
    pub supported: RangeInclusive<u32>,
}

/// Inputs for the changelog check.
pub struct ChangelogGate<'a> {
    pub summarizer: &'a dyn ChangelogSummarizer,
    /// Release notes between the current and the staged version, e.g. from
    /// [`super::changelog::changelog_text`].
    pub text: &'a str,
}

/// Everything the gate checks need. A check whose field is `None` does not
/// apply to the product and is skipped.
pub struct GateInputs<'a> {
    /// Executable of the version currently in use.
    pub old_executable: &'a Path,
    /// Executable of the staged version.
    pub new_executable: &'a Path,
    pub tools: Option<ToolGate<'a>>,
    pub formats: Option<FormatGate<'a>>,
    pub rpc: Option<RpcGate<'a>>,
    pub contract: Option<&'a dyn ContractRunner>,
    pub changelog: Option<ChangelogGate<'a>>,
}

/// Runs the applicable checks, querying ports concurrently. A port error
/// becomes [`CheckResult::failed_to_run`] for its check; the others still run.
pub async fn run_gates(
    product: &str,
    from: &Version,
    to: &Version,
    inputs: &GateInputs<'_>,
) -> GateReport {
    let old = inputs.old_executable;
    let new = inputs.new_executable;

    let tools = async {
        let gate = inputs.tools.as_ref()?;
        let (old_tools, new_tools) =
            futures::join!(gate.catalog.tools(old), gate.catalog.tools(new));
        Some(match (old_tools, new_tools) {
            (Ok(o), Ok(n)) => check_tool_schemas(&o, &n, gate.used),
            (Err(e), _) => failed(GateCheck::ToolSchemas, "current", e),
            (_, Err(e)) => failed(GateCheck::ToolSchemas, "staged", e),
        })
    };

    let session = async {
        let gate = inputs.formats.as_ref()?;
        let (old_version, new_version) =
            futures::join!(gate.probe.session_version(old), gate.probe.session_version(new));
        Some(match (old_version, new_version) {
            (Ok(o), Ok(n)) => check_session_format(o, n, gate.migrations),
            (Err(e), _) => failed(GateCheck::SessionFormat, "current", e),
            (_, Err(e)) => failed(GateCheck::SessionFormat, "staged", e),
        })
    };

    let config = async {
        let gate = inputs.formats.as_ref()?;
        let schema: Result<ConfigSchema, String> = gate.probe.config_keys(new).await;
        Some(match schema {
            Ok(schema) => check_config_schema(gate.written_keys, &schema),
            Err(e) => failed(GateCheck::ConfigSchema, "staged", e),
        })
    };

    let rpc = async {
        let gate = inputs.rpc.as_ref()?;
        Some(match gate.probe.protocol_versions(new).await {
            Ok(offered) => check_rpc_protocol(&offered, gate.supported.clone()),
            Err(e) => failed(GateCheck::RpcProtocol, "staged", e),
        })
    };

    let contract = async {
        let runner = inputs.contract?;
        Some(match runner.run(new).await {
            Ok(failures) => check_contract(&failures),
            Err(e) => failed(GateCheck::ContractTests, "staged", e),
        })
    };

    let changelog = async {
        let gate = inputs.changelog.as_ref()?;
        Some(match gate.summarizer.review(gate.text).await {
            Ok(review) => check_changelog(&review),
            Err(e) => CheckResult::failed_to_run(GateCheck::Changelog, e),
        })
    };

    let (tools, session, config, rpc, contract, changelog) =
        futures::join!(tools, session, config, rpc, contract, changelog);

    GateReport {
        product: product.to_string(),
        from: from.clone(),
        to: to.clone(),
        checks: [tools, session, config, rpc, contract, changelog].into_iter().flatten().collect(),
    }
}

fn failed(check: GateCheck, which: &str, error: String) -> CheckResult {
    CheckResult::failed_to_run(check, format!("{which} version: {error}"))
}
