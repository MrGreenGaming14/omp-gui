//! The harness registry: one [`HarnessFactory`] per compiled-in harness, so the
//! app builds adapters and asks about harness behavior by id instead of matching on names.

#[allow(unused_imports)]
use loa_contracts::harness::{Adapter, HarnessAccount, HarnessFactory, HarnessTraits};
use std::path::PathBuf;
use std::sync::Arc;

/// What the factories need from the app.
#[derive(Clone, Debug, Default)]
pub struct HarnessEnv {
    /// This node's key, written to `<session node>`.
    pub node: String,
    /// Where omp keeps its state for the app.
    pub omp_dir: PathBuf,
}

/// One registry entry: the harness's description and its build strategy.
struct Entry {
    id: &'static str,
    label: &'static str,
    icon: &'static str,
    program: &'static str,
    traits: HarnessTraits,
    build: fn(&HarnessEnv, Option<PathBuf>) -> Arc<dyn Adapter>,
    env: HarnessEnv,
}

impl HarnessFactory for Entry {
    fn id(&self) -> &'static str {
        self.id
    }

    fn label(&self) -> &'static str {
        self.label
    }

    fn icon_token(&self) -> &'static str {
        self.icon
    }

    fn default_program(&self) -> &'static str {
        self.program
    }

    fn build(&self, exe: Option<PathBuf>) -> Arc<dyn Adapter> {
        (self.build)(&self.env, exe)
    }

    fn account(&self, _exe: Option<PathBuf>) -> Option<Arc<dyn HarnessAccount>> {
        None
    }

    fn traits(&self) -> HarnessTraits {
        self.traits
    }
}

/// omp's `--approval-mode` values; the first is what app sessions start on.
pub const OMP_APPROVAL_MODES: &[&str] = &["always-ask", "always-ask-cached", "write", "yolo"];

/// ACP agents known by command line, tried in order.
#[cfg(feature = "acp")]
const ACP_AGENTS: [(&str, &[&str]); 2] = [("opencode", &["acp"]), ("gemini", &["--experimental-acp"])];

/// The factories of the compiled-in harnesses, in display order.
pub fn factories(env: &HarnessEnv) -> Vec<Arc<dyn HarnessFactory>> {
    let entry = |id, label, icon, program, traits, build| -> Arc<dyn HarnessFactory> { Arc::new(Entry { id, label, icon, program, traits, build, env: env.clone() }) };
    let mut out = Vec::new();
    #[cfg(feature = "omp")]
    out.push(entry(crate::omp::HARNESS, "omp", "harness.omp", "omp (from PATH)", HarnessTraits { approval_modes: OMP_APPROVAL_MODES, has_roles: true, server_url: false }, |env, exe| {
        let mut config = crate::omp::OmpAdapterConfig::new(&env.node, &env.omp_dir);
        config.approval_mode = Some(OMP_APPROVAL_MODES[0].to_owned());
        if let Some(exe) = exe {
            config.executable = exe;
        }
        Arc::new(crate::omp::OmpAdapter::new(config))
    }));
    #[cfg(feature = "claude")]
    out.push(entry(crate::claude::HARNESS, "Claude Code", "harness.claude", "claude (from PATH)", HarnessTraits::default(), |_, exe| {
        Arc::new(crate::claude::ClaudeAdapter::with_executable(exe.unwrap_or_else(|| "claude".into())))
    }));
    #[cfg(feature = "codex")]
    out.push(entry(crate::codex::HARNESS, "Codex", "harness.codex", "codex (from PATH)", HarnessTraits::default(), |env, exe| {
        Arc::new(crate::codex::CodexAdapter::new(env.node.as_str()).with_executable(exe.unwrap_or_else(|| "codex".into())))
    }));
    #[cfg(feature = "opencode")]
    out.push(entry(crate::opencode::HARNESS, "OpenCode", "harness.opencode", crate::opencode::DEFAULT_URL, HarnessTraits { server_url: true, ..HarnessTraits::default() }, |_, _| {
        Arc::new(crate::opencode::OpenCodeAdapter::new(crate::opencode::DEFAULT_URL, None))
    }));
    #[cfg(feature = "acp")]
    out.push(entry(crate::acp::HARNESS, "ACP", "harness.acp", "ACP agent (opencode or gemini from PATH)", HarnessTraits::default(), acp));
    #[cfg(feature = "omp2")]
    out.push(entry(crate::omp2::HARNESS, "omp2", "harness.omp", "omp (from PATH)", HarnessTraits::default(), |_, exe| {
        Arc::new(crate::omp2::Omp2Adapter::with_executable(exe.unwrap_or_else(|| crate::omp2::EXECUTABLE.into())))
    }));
    let _ = entry;
    out
}

/// The ACP agent `exe` (with a known agent's arguments when its name matches), else the first
/// known agent on `PATH`, else the first known one, which then reports what to install.
#[cfg(feature = "acp")]
fn acp(_: &HarnessEnv, exe: Option<PathBuf>) -> Arc<dyn Adapter> {
    use crate::acp::AcpAdapter;
    use std::path::Path;
    if let Some(program) = exe {
        let name = program.file_stem().and_then(|name| name.to_str()).map(str::to_owned);
        let args = ACP_AGENTS.iter().find(|(agent, _)| Some(*agent) == name.as_deref()).map_or(&[][..], |(_, args)| *args);
        return Arc::new(AcpAdapter::new(program, args.iter().copied()));
    }
    let installed = ACP_AGENTS.iter().find_map(|(name, args)| loa_util::exec::which(Path::new(name)).map(|path| (path, *args)));
    let (program, args) = installed.unwrap_or_else(|| (PathBuf::from(ACP_AGENTS[0].0), ACP_AGENTS[0].1));
    let names: Vec<_> = ACP_AGENTS.iter().map(|(name, _)| *name).collect();
    Arc::new(AcpAdapter::new(program, args.iter().copied()).with_missing_hint(format!("no ACP agent on PATH (looked for {})", names.join(", "))))
}
