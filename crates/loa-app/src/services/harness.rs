//! The harnesses the app can route sessions to, built from the
//! [`loa_harness::factories`] registry. Whether each can run here comes from
//! its own [`Adapter::availability`] probe, which the harness area runs at
//! startup and on demand.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use loa_contracts::harness::{Adapter, Availability, AvailabilityStatus, HarnessFactory, HarnessTraits, Registry};
use loa_harness::{HarnessEnv, factories};
use loa_ui_session::composer::HarnessOption;
use loa_util::exec::which as find_executable;

/// Picker note while a harness's probe hasn't answered.
pub const CHECKING: &str = "checking…";

/// One harness as the picker and the Harnesses tab list it.
pub struct Harness {
    pub option: HarnessOption,
    /// Executable passed in `SessionConfig::executable`; `None` lets the
    /// adapter run its default.
    pub executable: Option<PathBuf>,
    /// The executable `[harness.executables]` names instead of the default.
    pub configured: Option<PathBuf>,
    /// What the last probe found; `None` until it answers.
    pub availability: Option<Availability>,
    pub traits: HarnessTraits,
    /// What runs when nothing is configured (the server URL of a server harness).
    pub program: &'static str,
}

/// Every harness the app knows, with their adapters.
pub struct Harnesses {
    /// Every adapter, runnable or not; [`Harnesses::runnable`] picks the
    /// ones that can start.
    pub registry: Registry,
    pub list: Vec<Harness>,
}

fn factory(id: &str, env: &HarnessEnv) -> Option<Arc<dyn HarnessFactory>> {
    factories(env).into_iter().find(|factory| factory.id() == id)
}

impl Harnesses {
    /// Registers every harness of the registry on its default executable.
    /// Until their probes answer, CLI harnesses count as available when
    /// their executable is on `PATH`.
    pub fn detect(node: &str, omp_dir: &Path) -> Self {
        let mut harnesses = Self { registry: Registry::new(), list: Vec::new() };
        for factory in factories(&HarnessEnv { node: node.to_owned(), omp_dir: omp_dir.to_owned() }) {
            harnesses.register(factory.as_ref(), None);
        }
        harnesses
    }

    /// Registers `adapter` and lists it as `label`, `available` (or not,
    /// for `detail`) until [`Harnesses::set_availability`] reports a probe.
    pub fn add(&mut self, adapter: Arc<dyn Adapter>, label: &str, available: bool, detail: Option<String>, executable: Option<PathBuf>) {
        let id = adapter.harness();
        self.registry.register(id, adapter);
        let known = factory(id, &HarnessEnv::default());
        let harness = Harness {
            option: HarnessOption { id: id.to_owned(), label: label.to_owned(), available, detail },
            executable,
            configured: None,
            availability: None,
            traits: known.as_ref().map(|f| f.traits()).unwrap_or_default(),
            program: known.as_ref().map_or("", |f| f.default_program()),
        };
        match self.list.iter_mut().find(|listed| listed.option.id == id) {
            Some(listed) => *listed = harness,
            None => self.list.push(harness),
        }
    }

    /// (Re)registers the known harness `id` running `executable`, or its
    /// default when `None`; it waits for a new probe. Returns whether `id`
    /// is known.
    pub fn set_executable(&mut self, id: &str, executable: Option<PathBuf>, node: &str, omp_dir: &Path) -> bool {
        let Some(factory) = factory(id, &HarnessEnv { node: node.to_owned(), omp_dir: omp_dir.to_owned() }) else { return false };
        self.register(factory.as_ref(), executable);
        true
    }

    fn register(&mut self, factory: &dyn HarnessFactory, executable: Option<PathBuf>) {
        let default = factory.default_program().split_whitespace().next().map(PathBuf::from);
        let program = executable.clone().or(default).filter(|_| !factory.traits().server_url);
        let found = program.as_deref().and_then(find_executable);
        self.add(factory.build(executable.clone()), factory.label(), found.is_some(), Some(CHECKING.to_owned()), found);
        if let Some(harness) = self.list.iter_mut().find(|harness| harness.option.id == factory.id()) {
            harness.configured = executable;
        }
    }

    /// Records what harness `id`'s probe found: whether it can be picked,
    /// the note beside it and the executable sessions run. An adapter that
    /// can't tell keeps what it was registered with.
    pub fn set_availability(&mut self, id: &str, availability: Availability) {
        let Some(harness) = self.list.iter_mut().find(|harness| harness.option.id == id) else { return };
        if availability.status != AvailabilityStatus::Unknown {
            harness.option.available = availability.is_ready();
            harness.executable.clone_from(&availability.executable);
        }
        harness.option.detail.clone_from(&availability.detail);
        harness.availability = Some(availability);
    }

    /// The adapter of harness `id` when it can start sessions here.
    pub fn runnable(&self, id: &str) -> Option<Arc<dyn Adapter>> {
        self.get(id).filter(|harness| harness.option.available).and_then(|_| self.registry.get(id))
    }

    /// The picker options, in display order.
    pub fn options(&self) -> Vec<HarnessOption> {
        self.list.iter().map(|harness| harness.option.clone()).collect()
    }

    pub fn get(&self, id: &str) -> Option<&Harness> {
        self.list.iter().find(|harness| harness.option.id == id)
    }

    /// What the app asks of harness `id`; the defaults for one it doesn't know.
    pub fn traits(&self, id: &str) -> HarnessTraits {
        self.get(id).map(|harness| harness.traits).unwrap_or_default()
    }

    /// Label of the harness with this id, or the id itself.
    pub fn label<'a>(&'a self, id: &'a str) -> &'a str {
        self.get(id).map_or(id, |harness| harness.option.label.as_str())
    }

    /// The first harness with model roles (omp) when it can run, else the first that can.
    pub fn default_id(&self) -> Option<String> {
        let available = || self.list.iter().filter(|harness| harness.option.available);
        available()
            .find(|harness| harness.traits.has_roles)
            .or_else(|| available().next())
            .map(|harness| harness.option.id.clone())
    }
}
