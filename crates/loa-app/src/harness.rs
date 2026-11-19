//! The harnesses the app can route sessions to, with availability detected
//! from this machine.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use loa_adapter::{Adapter, Registry};
use loa_ui_composer::HarnessOption;

use crate::host::which;

/// Address `opencode serve` listens on by default.
const OPENCODE_ADDR: ([u8; 4], u16) = ([127, 0, 0, 1], 4096);

/// How long the OpenCode probe waits for the server to accept.
const OPENCODE_PROBE: Duration = Duration::from_millis(150);

/// ACP agents known by command line, tried in order.
const ACP_AGENTS: [(&str, &[&str]); 2] = [("opencode", &["acp"]), ("gemini", &["--experimental-acp"])];

/// One harness as the picker lists it.
pub struct Harness {
    pub option: HarnessOption,
    /// Executable passed in `SessionConfig::executable`; `None` lets the
    /// adapter run its default from `PATH`.
    pub executable: Option<PathBuf>,
}

/// Every harness, and the adapters of those that can run.
pub struct Harnesses {
    pub registry: Registry,
    pub list: Vec<Harness>,
}

impl Harnesses {
    /// Detects each harness: omp, Claude Code, Codex and omp2 by their
    /// executable on `PATH`, OpenCode by a server on its default port, ACP by
    /// a known ACP agent on `PATH`.
    pub fn detect(node: &str, state_dir: &Path) -> Self {
        let mut harnesses = Self { registry: Registry::new(), list: Vec::new() };
        let omp = loa_adapter_omp::OmpAdapter::new(loa_adapter_omp::OmpAdapterConfig::new(node, state_dir));
        harnesses.add_cli(Arc::new(omp), "omp", "omp", false);
        harnesses.add_cli(Arc::new(loa_adapter_claude::ClaudeAdapter::new()), "Claude Code", "claude", false);
        harnesses.add_cli(Arc::new(loa_adapter_codex::CodexAdapter::new(node)), "Codex", "codex", false);

        let addr = SocketAddr::from(OPENCODE_ADDR);
        let serving = TcpStream::connect_timeout(&addr, OPENCODE_PROBE).is_ok();
        let opencode = loa_adapter_opencode::OpenCodeAdapter::new(format!("http://{addr}"), None);
        let detail = (!serving).then(|| format!("start `opencode serve` on {addr}"));
        harnesses.add(Arc::new(opencode), "OpenCode", serving, detail, None);

        match ACP_AGENTS.iter().find_map(|(name, args)| which(name).map(|path| (path, *name, *args))) {
            Some((path, name, args)) => {
                let detail = Some(format!("{name} {}", args.join(" ")));
                harnesses.add(Arc::new(loa_adapter_acp::AcpAdapter::new(path, args.iter().copied())), "ACP", true, detail, None);
            }
            None => harnesses.list.push(Harness {
                option: HarnessOption {
                    id: loa_adapter_acp::HARNESS.to_owned(),
                    label: "ACP".to_owned(),
                    available: false,
                    detail: Some("no ACP agent on PATH".to_owned()),
                },
                executable: None,
            }),
        }

        harnesses.add_cli(Arc::new(loa_adapter_omp2::Omp2Adapter::new()), "omp2", "omp2", true);
        harnesses
    }

    /// Adds a harness whose CLI must be on `PATH`. `pin` passes the found
    /// path to the adapter, for adapters whose default executable differs.
    fn add_cli(&mut self, adapter: Arc<dyn Adapter>, label: &str, program: &str, pin: bool) {
        let found = which(program);
        let detail = found.is_none().then(|| format!("`{program}` not on PATH"));
        let executable = found.as_ref().filter(|_| pin).cloned();
        self.add(adapter, label, found.is_some(), detail, executable);
    }

    pub fn add(&mut self, adapter: Arc<dyn Adapter>, label: &str, available: bool, detail: Option<String>, executable: Option<PathBuf>) {
        let id = adapter.harness();
        if available {
            self.registry.register(adapter);
        }
        self.list.push(Harness {
            option: HarnessOption { id: id.to_owned(), label: label.to_owned(), available, detail },
            executable,
        });
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
