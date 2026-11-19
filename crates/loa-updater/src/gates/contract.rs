//! Adapter contract tests: golden sessions round tripped through a version.

use super::{CheckResult, GateCheck};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One contract test whose output no longer matches its golden file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractFailure {
    pub test: String,
    /// Line diff between expected and actual output.
    pub diff: String,
}

/// Runs the adapter contract tests against an installed version.
#[async_trait::async_trait]
pub trait ContractRunner: Send + Sync {
    async fn run(&self, executable: &Path) -> Result<Vec<ContractFailure>, String>;
}

/// Harmful when any contract test failed; details list each test and its diff.
pub fn check_contract(failures: &[ContractFailure]) -> CheckResult {
    if failures.is_empty() {
        return CheckResult::pass(GateCheck::ContractTests, Vec::new());
    }
    let mut details = Vec::new();
    for failure in failures {
        details.push(format!("{}:", failure.test));
        details.extend(failure.diff.lines().map(|line| format!("  {line}")));
    }
    CheckResult::harmful(GateCheck::ContractTests, details)
}
