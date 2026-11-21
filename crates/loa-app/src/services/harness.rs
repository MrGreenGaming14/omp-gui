//! The runtime's harnesses, built from the [`loa_harness::factories`]
//! registry, and the composer's picker over them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use loa_contracts::harness::{Adapter, HarnessFactory};
use loa_harness::{HarnessEnv, factories};
use loa_runtime::harnesses::{Harness, Harnesses};
use loa_ui_session::composer::HarnessOption;
use loa_util::exec::which as find_executable;

/// Picker note while a harness's probe hasn't answered.
pub const CHECKING: &str = "checking…";

fn factory(id: &str, env: &HarnessEnv) -> Option<Arc<dyn HarnessFactory>> {
    factories(env).into_iter().find(|factory| factory.id() == id)
}

/// Registers every harness of the registry on its default executable.
pub fn detect(node: &str, omp_dir: &Path) -> Harnesses {
    let mut harnesses = Harnesses::default();
    for factory in factories(&HarnessEnv { node: node.to_owned(), omp_dir: omp_dir.to_owned() }) {
        register(&mut harnesses, factory.as_ref(), None);
    }
    harnesses
}

/// Registers `adapter` and lists it as `label`, `available` (or not, for
/// `detail`) until [`Harnesses::set_availability`] reports a probe; a
/// harness the registry knows gets its traits and default program.
pub fn add(harnesses: &mut Harnesses, adapter: Arc<dyn Adapter>, label: &str, available: bool, detail: Option<String>, executable: Option<PathBuf>) {
    let id = adapter.harness().to_owned();
    let known = factory(&id, &HarnessEnv::default());
    let harness = Harness {
        id,
        label: label.to_owned(),
        available,
        detail,
        executable,
        configured: None,
        availability: None,
        traits: known.as_ref().map(|f| f.traits()).unwrap_or_default(),
        program: known.as_ref().map_or("", |f| f.default_program()),
    };
    harnesses.add(adapter, harness);
}

/// (Re)registers the known harness `id` running `executable`, or its
/// default when `None`; it waits for a new probe.
pub fn set_executable(harnesses: &mut Harnesses, id: &str, executable: Option<PathBuf>, node: &str, omp_dir: &Path) -> bool {
    let Some(factory) = factory(id, &HarnessEnv { node: node.to_owned(), omp_dir: omp_dir.to_owned() }) else { return false };
    register(harnesses, factory.as_ref(), executable);
    true
}

/// Registers each known harness of `executables` (`[harness] executables`)
/// running its executable there; the ids it registered.
pub fn set_executables<'a>(harnesses: &mut Harnesses, executables: &'a BTreeMap<String, PathBuf>, node: &str, omp_dir: &Path) -> Vec<&'a str> {
    executables.iter().filter(|(id, executable)| set_executable(harnesses, id, Some((*executable).clone()), node, omp_dir)).map(|(id, _)| id.as_str()).collect()
}

fn register(harnesses: &mut Harnesses, factory: &dyn HarnessFactory, executable: Option<PathBuf>) {
    let default = factory.default_program().split_whitespace().next().map(PathBuf::from);
    let program = executable.clone().or(default).filter(|_| !factory.traits().server_url);
    let found = program.as_deref().and_then(find_executable);
    add(harnesses, factory.build(executable.clone()), factory.label(), found.is_some(), Some(CHECKING.to_owned()), found);
    if let Some(harness) = harnesses.list.iter_mut().find(|harness| harness.id == factory.id()) {
        harness.configured = executable;
    }
}

/// The picker options, in display order.
pub fn options(harnesses: &Harnesses) -> Vec<HarnessOption> {
    harnesses
        .list
        .iter()
        .map(|harness| HarnessOption {
            id: harness.id.clone(),
            label: harness.label.clone(),
            available: harness.available,
            detail: harness.detail.clone(),
        })
        .collect()
}
