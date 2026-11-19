//! The whole contract for one adapter in one call: capabilities, round trips
//! of native fixtures, and live runs. The updater runs it against an adapter
//! configured with a staged harness executable before switching versions.

use crate::caps::check_capabilities;
use crate::live::{Action, LiveDriver, LiveReport};
use crate::roundtrip::{RoundTrip, round_trip};
use crate::violation::{Rule, Violation, write_list};
use crate::{Error, Result};
use loa_adapter::{Adapter, SessionConfig};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// One live run.
#[derive(Clone, Debug)]
pub struct LivePlan {
    pub name: String,
    /// Points `executable` at the harness to test (a fake, or a staged
    /// version that is itself driven by a fake model backend).
    pub config: SessionConfig,
    pub actions: Vec<Action>,
}

/// What to run.
#[derive(Clone, Debug)]
pub struct Contract {
    /// Native session files or directories, round tripped one by one. The
    /// first also proves a declared `import` works.
    pub fixtures: Vec<PathBuf>,
    pub live: Vec<LivePlan>,
    /// Per-step limit of the live runs.
    pub timeout: Duration,
}

/// Everything [`run_contract`] found.
#[derive(Debug)]
pub struct ContractReport {
    pub harness: &'static str,
    pub capabilities: Vec<Violation>,
    pub round_trips: Vec<RoundTrip>,
    /// Fixtures whose round trip couldn't run: the adapter failed to import or export.
    pub errors: Vec<Violation>,
    pub live: Vec<(String, LiveReport)>,
}

impl ContractReport {
    pub fn is_clean(&self) -> bool {
        self.capabilities.is_empty()
            && self.errors.is_empty()
            && self.round_trips.iter().all(RoundTrip::is_clean)
            && self.live.iter().all(|(_, r)| r.is_clean())
    }
}

impl fmt::Display for ContractReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "contract of {}:", self.harness)?;
        write!(f, "capabilities: ")?;
        write_list(f, &self.capabilities)?;
        if !self.errors.is_empty() {
            write!(f, "round trip errors: ")?;
            write_list(f, &self.errors)?;
        }
        for rt in &self.round_trips {
            write!(f, "{rt}")?;
        }
        for (name, live) in &self.live {
            write!(f, "{name}: {live}")?;
        }
        Ok(())
    }
}

/// Runs every check in `contract` against `adapter`.
pub async fn run_contract(adapter: Arc<dyn Adapter>, contract: &Contract) -> Result<ContractReport> {
    let scratch = tempfile::tempdir().map_err(|source| Error::io(Path::new("temp dir"), source))?;
    let capabilities = check_capabilities(&*adapter, contract.fixtures.first().map(PathBuf::as_path), scratch.path());
    let mut round_trips = Vec::new();
    let mut errors = Vec::new();
    for fixture in &contract.fixtures {
        match round_trip(&*adapter, fixture) {
            Ok(rt) => round_trips.push(rt),
            Err(err) => errors.push(Violation::new(Rule::Adapter, None, format!("{}: {err}", fixture.display()))),
        }
    }
    let mut live = Vec::new();
    for plan in &contract.live {
        let driver = LiveDriver::new(Arc::clone(&adapter), plan.config.clone()).with_timeout(contract.timeout);
        live.push((plan.name.clone(), driver.run(&plan.actions).await));
    }
    Ok(ContractReport { harness: adapter.harness(), capabilities, round_trips, errors, live })
}
