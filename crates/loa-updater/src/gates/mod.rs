//! Gate checks run against a staged version before it may be swapped in.
//!
//! Each check is a function returning a [`CheckResult`]. A check whose port
//! fails to answer counts as harmful: an update is only clean when every
//! check positively passed.

pub mod changelog;
pub mod contract;
pub mod formats;
pub mod rpc;
pub mod run;
pub mod tools;

pub use changelog::{
    ChangelogReview, ChangelogSummarizer, OmpSmolSummarizer, changelog_text, check_changelog,
    parse_review,
};
pub use contract::{ContractFailure, ContractRunner, check_contract};
pub use formats::{ConfigSchema, FormatProbe, check_config_schema, check_session_format};
pub use rpc::{OmpReadyProbe, RpcProbe, check_rpc_protocol};
pub use run::{ChangelogGate, FormatGate, GateInputs, RpcGate, ToolGate, run_gates};
pub use tools::{ToolCatalog, ToolSchema, UsedTools, check_tool_schemas};

use semver::Version;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Which gate check a result belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateCheck {
    ToolSchemas,
    SessionFormat,
    ConfigSchema,
    RpcProtocol,
    ContractTests,
    Changelog,
}

impl GateCheck {
    /// Short name shown in the report.
    pub fn label(self) -> &'static str {
        match self {
            Self::ToolSchemas => "Tool schemas",
            Self::SessionFormat => "Session format version",
            Self::ConfigSchema => "Config schema",
            Self::RpcProtocol => "RPC protocol version",
            Self::ContractTests => "Adapter contract tests",
            Self::Changelog => "Changelog",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Harmful,
}

/// Outcome of one check, with human readable lines explaining it (a diff
/// for schema checks, failures for tests, flagged entries for changelogs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub check: GateCheck,
    pub verdict: Verdict,
    pub details: Vec<String>,
}

impl CheckResult {
    pub fn pass(check: GateCheck, details: Vec<String>) -> Self {
        Self { check, verdict: Verdict::Pass, details }
    }

    pub fn harmful(check: GateCheck, details: Vec<String>) -> Self {
        Self { check, verdict: Verdict::Harmful, details }
    }

    /// The check couldn't run (its port failed). Counts as harmful.
    pub fn failed_to_run(check: GateCheck, error: impl fmt::Display) -> Self {
        Self::harmful(check, vec![format!("check could not run: {error}")])
    }
}

/// All gate results for one staged update.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateReport {
    pub product: String,
    pub from: Version,
    pub to: Version,
    pub checks: Vec<CheckResult>,
}

impl GateReport {
    /// True when every check passed.
    pub fn is_clean(&self) -> bool {
        self.checks.iter().all(|c| c.verdict == Verdict::Pass)
    }

    pub fn harmful(&self) -> impl Iterator<Item = &CheckResult> {
        self.checks.iter().filter(|c| c.verdict == Verdict::Harmful)
    }
}

impl fmt::Display for GateReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} {} -> {}", self.product, self.from, self.to)?;
        for check in &self.checks {
            let verdict = match check.verdict {
                Verdict::Pass => "pass",
                Verdict::Harmful => "HARMFUL",
            };
            writeln!(f, "  {}: {verdict}", check.check.label())?;
            for line in &check.details {
                writeln!(f, "    {line}")?;
            }
        }
        Ok(())
    }
}
