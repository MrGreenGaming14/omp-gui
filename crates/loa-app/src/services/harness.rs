//! The harnesses the app can route sessions to. Every adapter is registered;
//! whether each can run here comes from its own
//! [`Adapter::availability`] probe, which the harness area runs at startup
//! and on demand.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use loa_contracts::harness::{Adapter, Availability, AvailabilityStatus, Registry};
use loa_util::exec::which as find_executable;
use loa_ui_composer::HarnessOption;

/// Address `opencode serve` listens on by default.
pub const OPENCODE_URL: &str = "http://127.0.0.1:4096";

/// ACP agents known by command line, tried in order. The first is used
/// when none is installed, so the probe names what to install.
const ACP_AGENTS: [(&str, &[&str]); 2] = [("opencode", &["acp"]), ("gemini", &["--experimental-acp"])];

/// omp's `--approval-mode` for app sessions: every tool call that needs
/// approval reaches the app as an inline decision card.
const OMP_APPROVAL_MODE: &str = "always-ask";

/// Picker note while a harness's probe hasn't answered.
pub const CHECKING: &str = "checking…";

/// Every harness the app ships an adapter for: id and label, in display
/// order.
pub const KNOWN: [(&str, &str); 6] = [
    ("omp", "omp"),
    (loa_adapter_claude::HARNESS, "Claude Code"),
    (loa_adapter_codex::HARNESS, "Codex"),
    (loa_adapter_opencode::mapper::HARNESS, "OpenCode"),
    (loa_harness::acp::HARNESS, "ACP"),
    (loa_adapter_omp2::HARNESS, "omp2"),
];

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
}

/// Every harness the app knows, with their adapters.
pub struct Harnesses {
    /// Every adapter, runnable or not; [`Harnesses::runnable`] picks the
    /// ones that can start.
    pub registry: Registry,
    pub list: Vec<Harness>,
}

impl Harnesses {
    /// Registers every [`KNOWN`] harness on its default executable: omp,
    /// Claude Code, Codex, OpenCode (a server on its default port), ACP (the
    /// first known ACP agent on `PATH`) and omp2. Until their probes answer,
    /// CLI harnesses count as available when their executable is on
    /// `PATH`. `node` is this node's key, written to `<session node>`; omp
    /// keeps its state under `omp_dir`.
    pub fn detect(node: &str, omp_dir: &Path) -> Self {
        let mut harnesses = Self { registry: Registry::new(), list: Vec::new() };
        for (id, _) in KNOWN {
            harnesses.set_executable(id, None, node, omp_dir);
        }
        harnesses
    }

    /// Registers `adapter` and lists it as `label`, `available` (or not,
    /// for `detail`) until [`Harnesses::set_availability`] reports a probe.
    pub fn add(&mut self, adapter: Arc<dyn Adapter>, label: &str, available: bool, detail: Option<String>, executable: Option<PathBuf>) {
        let id = adapter.harness();
        self.registry.register(id, adapter);
        let harness = Harness {
            option: HarnessOption { id: id.to_owned(), label: label.to_owned(), available, detail },
            executable,
            configured: None,
            availability: None,
        };
        match self.list.iter_mut().find(|listed| listed.option.id == id) {
            Some(listed) => *listed = harness,
            None => self.list.push(harness),
        }
    }

    /// (Re)registers the [`KNOWN`] harness `id` running `executable`, or its
    /// default when `None`; it waits for a new probe. Returns whether `id`
    /// is known.
    pub fn set_executable(&mut self, id: &str, executable: Option<PathBuf>, node: &str, omp_dir: &Path) -> bool {
        let Some((id, label)) = KNOWN.into_iter().find(|(known, _)| *known == id) else { return false };
        let (adapter, program): (Arc<dyn Adapter>, Option<PathBuf>) = match id {
            "omp" => {
                let mut omp = loa_harness::omp::OmpAdapterConfig::new(node, omp_dir);
                omp.approval_mode = Some(OMP_APPROVAL_MODE.to_owned());
                if let Some(executable) = &executable {
                    omp.executable.clone_from(executable);
                }
                let program = omp.executable.clone();
                (Arc::new(loa_harness::omp::OmpAdapter::new(omp)), Some(program))
            }
            loa_adapter_claude::HARNESS => {
                let program = executable.clone().unwrap_or_else(|| "claude".into());
                (Arc::new(loa_adapter_claude::ClaudeAdapter::with_executable(program.clone())), Some(program))
            }
            loa_adapter_codex::HARNESS => {
                let program = executable.clone().unwrap_or_else(|| "codex".into());
                (Arc::new(loa_adapter_codex::CodexAdapter::new(node).with_executable(program.clone())), Some(program))
            }
            loa_adapter_opencode::mapper::HARNESS => {
                (Arc::new(loa_adapter_opencode::OpenCodeAdapter::new(OPENCODE_URL.to_owned(), None)), None)
            }
            loa_harness::acp::HARNESS => {
                let (program, args) = acp_agent(executable.as_deref());
                (Arc::new(loa_harness::acp::AcpAdapter::new(program, args.iter().copied())), None)
            }
            _ => {
                let program = executable.clone().unwrap_or_else(|| loa_adapter_omp2::EXECUTABLE.into());
                (Arc::new(loa_adapter_omp2::Omp2Adapter::with_executable(program.clone())), Some(program))
            }
        };
        let found = program.as_deref().and_then(find_executable);
        self.add(adapter, label, found.is_some(), Some(CHECKING.to_owned()), found);
        if let Some(harness) = self.list.iter_mut().find(|harness| harness.option.id == id) {
            harness.configured = executable;
        }
        true
    }

    /// Records what harness `id`'s probe found: whether it can be picked,
    /// the note beside it and the executable sessions run. An adapter that
    /// can't tell keeps what it was registered with.
    pub fn set_availability(&mut self, id: &str, mut availability: Availability) {
        let Some(harness) = self.list.iter_mut().find(|harness| harness.option.id == id) else { return };
        // Without a configured agent the probe tried only the fallback name.
        if id == loa_harness::acp::HARNESS && harness.configured.is_none() && availability.status == AvailabilityStatus::NotInstalled {
            let names: Vec<_> = ACP_AGENTS.iter().map(|(name, _)| *name).collect();
            availability.detail = Some(format!("no ACP agent on PATH (looked for {})", names.join(", ")));
        }
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

    /// Label of the harness with this id, or the id itself.
    pub fn label<'a>(&'a self, id: &'a str) -> &'a str {
        self.get(id).map_or(id, |harness| harness.option.label.as_str())
    }

    /// omp when it can run, else the first harness that can.
    pub fn default_id(&self) -> Option<String> {
        let available = || self.list.iter().filter(|harness| harness.option.available);
        available()
            .find(|harness| harness.option.id == "omp")
            .or_else(|| available().next())
            .map(|harness| harness.option.id.clone())
    }
}

/// The ACP agent to run and its arguments: `program` when configured (with
/// a known agent's arguments when its name matches), else the first known
/// agent on `PATH`, else the first known one.
fn acp_agent(program: Option<&Path>) -> (PathBuf, &'static [&'static str]) {
    if let Some(program) = program {
        let name = program.file_stem().and_then(|name| name.to_str());
        let args = ACP_AGENTS.iter().find(|(agent, _)| Some(*agent) == name).map_or(&[][..], |(_, args)| *args);
        return (program.to_owned(), args);
    }
    let installed = ACP_AGENTS.iter().find_map(|(name, args)| find_executable(Path::new(name)).map(|path| (path, *args)));
    installed.unwrap_or_else(|| (PathBuf::from(ACP_AGENTS[0].0), ACP_AGENTS[0].1))
}
