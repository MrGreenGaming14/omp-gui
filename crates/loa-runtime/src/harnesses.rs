//! The harnesses the runtime can route sessions to.

use std::path::PathBuf;
use std::sync::Arc;

use loa_contracts::harness::{Adapter, Availability, AvailabilityStatus, HarnessTraits, Registry};

/// One harness as the picker and the Harnesses tab list it.
pub struct Harness {
    /// Harness id, e.g. `omp`; the id its adapter registers under.
    pub id: String,
    pub label: String,
    /// It can be picked: its probe found it ready.
    pub available: bool,
    /// Note beside it: why it can't run, or what the probe found.
    pub detail: Option<String>,
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

/// Every harness the runtime knows, with their adapters.
#[derive(Default)]
pub struct Harnesses {
    /// Every adapter, runnable or not; [`Harnesses::runnable`] picks the
    /// ones that can start.
    pub registry: Registry,
    pub list: Vec<Harness>,
}

impl Harnesses {
    /// Registers `adapter` and lists it as `harness`, replacing the entry
    /// with its id, until [`Harnesses::set_availability`] reports a probe.
    /// `harness.id` is the adapter's [`Adapter::harness`].
    pub fn add(&mut self, adapter: Arc<dyn Adapter>, harness: Harness) {
        debug_assert_eq!(adapter.harness(), harness.id, "a harness is listed under its adapter's id");
        self.registry.register(harness.id.clone(), adapter);
        match self.list.iter_mut().find(|listed| listed.id == harness.id) {
            Some(listed) => *listed = harness,
            None => self.list.push(harness),
        }
    }

    /// Records what harness `id`'s probe found: whether it can be picked,
    /// the note beside it and the executable sessions run.
    pub fn set_availability(&mut self, id: &str, availability: Availability) {
        let Some(harness) = self.list.iter_mut().find(|harness| harness.id == id) else { return };
        if availability.status != AvailabilityStatus::Unknown {
            harness.available = availability.is_ready();
            harness.executable.clone_from(&availability.executable);
        }
        harness.detail.clone_from(&availability.detail);
        harness.availability = Some(availability);
    }

    /// The adapter of harness `id` when it can start sessions here.
    pub fn runnable(&self, id: &str) -> Option<Arc<dyn Adapter>> {
        self.get(id).filter(|harness| harness.available).and_then(|_| self.registry.get(id))
    }

    pub fn get(&self, id: &str) -> Option<&Harness> {
        self.list.iter().find(|harness| harness.id == id)
    }

    /// What the app asks of harness `id`; the defaults for one it doesn't know.
    pub fn traits(&self, id: &str) -> HarnessTraits {
        self.get(id).map(|harness| harness.traits).unwrap_or_default()
    }

    /// Label of the harness with this id, or the id itself.
    pub fn label<'a>(&'a self, id: &'a str) -> &'a str {
        self.get(id).map_or(id, |harness| harness.label.as_str())
    }

    /// The harness whose model catalog and roles the models area shows (omp); empty when none is compiled in.
    pub fn roles_id(&self) -> &str {
        self.list.iter().find(|harness| harness.traits.has_roles).map_or("", |harness| harness.id.as_str())
    }

    /// The first harness with model roles (omp) when it can run, else the first that can.
    pub fn default_id(&self) -> Option<String> {
        let available = || self.list.iter().filter(|harness| harness.available);
        available().find(|harness| harness.traits.has_roles).or_else(|| available().next()).map(|harness| harness.id.clone())
    }
}
