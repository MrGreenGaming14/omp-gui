//! The real model screens' backend, composed from `loa-harness`.

use std::sync::Arc;

use loa_contracts::harness::HarnessFactory;
use loa_harness::account_support::opener::SystemUrlOpener;
use loa_harness::account_support::process::TokioProcessRunner;
use loa_harness::omp::account::{OmpAccount, OmpLaunch, OmpService};
use loa_harness::omp::validate::OmpSettingsValidator;
use loa_models::keys::ProcessEnvironment;
use loa_models::service::{FilesSource, OMP, Parts};
use loa_runtime::harnesses::Harnesses;

/// The model screens' backend parts: omp started from `launch` (one RPC
/// process, owned by omp's account), omp's active agent directory,
/// validation by omp, the platform browser and the accounts of the other
/// harnesses `factories` know, each on the executable `harnesses` runs it
/// with.
pub fn models_parts(launch: OmpLaunch, harnesses: &Harnesses, factories: &[Arc<dyn HarnessFactory>]) -> Parts {
    let rpc = Arc::new(OmpService::new(launch.clone()));
    let opener = Arc::new(SystemUrlOpener);
    let omp_account = Arc::new(OmpAccount { rpc: rpc.clone(), opener: opener.clone(), launch: launch.clone() });
    let accounts = factories
        .iter()
        .filter(|factory| factory.id() != OMP)
        .filter_map(|factory| factory.account(harnesses.get(factory.id()).and_then(|harness| harness.executable.clone())))
        .collect();
    Parts {
        rpc,
        omp_account,
        files: FilesSource::Resolve(Arc::new(launch.clone())),
        validator: Arc::new(OmpSettingsValidator::new(launch.executable, Arc::new(TokioProcessRunner))),
        opener,
        harnesses: accounts,
        environment: Arc::new(ProcessEnvironment),
    }
}
