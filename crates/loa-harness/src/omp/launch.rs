//! How an app managed omp process is started: the memory switch, project
//! context delivery and session selection, as CLI arguments.

use super::rpc::{Mode, SpawnOptions};
use loa_contracts::harness::{AdapterError, Result, SessionConfig};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Who gives the session its cross-session memory. One switch, so a session
/// never receives two summaries.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum MemoryMode {
    /// The app delivers project digests (`SessionConfig::context`) through
    /// `--append-system-prompt`, and omp's own memory backend is forced off
    /// for the process with a `--config` overlay (`memory.backend: off`).
    #[default]
    AppDigests,
    /// omp's memory backend runs as the user configured it and the app
    /// delivers no project context; `SessionConfig::context` is ignored.
    OmpMemory,
}

/// Settings of the omp adapter that aren't per session.
#[derive(Clone, Debug)]
pub struct OmpAdapterConfig {
    /// Owner node key written to `<session node>` of imported sessions.
    pub node: String,
    /// The executable used when `SessionConfig::executable` is unset.
    pub executable: PathBuf,
    pub memory: MemoryMode,
    /// A directory the adapter may write to: the memory overlay and per
    /// session context files live here.
    pub state_dir: PathBuf,
    /// Passed as `--approval-mode` (`always-ask`, `always-ask-cached`,
    /// `write`, `yolo`) so tool calls that need approval reach the app as
    /// decision cards. `None` keeps omp's configured mode.
    pub approval_mode: Option<String>,
    /// Passed as `--session-dir` so omp keeps app managed sessions apart.
    /// `None` uses omp's default location.
    pub session_dir: Option<PathBuf>,
    /// How long `start` waits for omp's `ready` frame.
    pub ready_timeout: Duration,
    /// How long `shutdown` waits for omp to exit after closing its stdin.
    pub shutdown_grace: Duration,
}

impl OmpAdapterConfig {
    pub fn new(node: impl Into<String>, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            node: node.into(),
            executable: PathBuf::from("omp"),
            memory: MemoryMode::AppDigests,
            state_dir: state_dir.into(),
            approval_mode: None,
            session_dir: None,
            ready_timeout: Duration::from_secs(60),
            shutdown_grace: Duration::from_secs(10),
        }
    }
}

/// omp's built-in tools that are on by default (`omp --help`, "Built-in
/// Tools"). A tool its settings switch off stays off when listed.
pub const DEFAULT_TOOLS: &[&str] = &[
    "read", "edit", "write", "bash", "grep", "glob", "find", "eval", "task", "wait", "todo", "ask", "web_search", "lsp", "ast_edit", "debug",
];

/// The session omp should open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    New,
    /// `--resume <id or path>`.
    Resume(String),
}

/// Builds the spawn options for one session and writes the files they
/// reference (memory overlay, context file).
pub fn spawn_options(adapter: &OmpAdapterConfig, session: &SessionConfig, target: &Target) -> Result<SpawnOptions> {
    let executable = session.executable.clone().unwrap_or_else(|| adapter.executable.clone());
    let mut opts = SpawnOptions::new(executable);
    // `rpc-ui` keeps the tool UI context, so the `ask` tool reaches the app.
    opts.mode = Mode::RpcUi;
    opts.ready_timeout = adapter.ready_timeout;
    opts.cwd = Some(session.cwd.clone());
    opts.env = session.env.iter().map(|(k, v)| (k.into(), v.into())).collect();

    match adapter.memory {
        MemoryMode::AppDigests => {
            let overlay = write_state_file(&adapter.state_dir, "memory-off.yml", super::MEMORY_OFF)?;
            opts.args.push("--config".into());
            opts.args.push(overlay.into());
        }
        MemoryMode::OmpMemory => {}
    }
    let context = (adapter.memory == MemoryMode::AppDigests).then_some(session.context.as_deref()).flatten().filter(|s| !s.trim().is_empty());
    let instructions = session.instructions.as_deref().filter(|s| !s.trim().is_empty());
    let prompt = match (context, instructions) {
        (Some(context), Some(instructions)) => Some(format!("{context}\n\n{instructions}")),
        (Some(text), None) | (None, Some(text)) => Some(text.to_owned()),
        (None, None) => None,
    };
    if let Some(prompt) = prompt {
        let name = format!("context-{}.md", file_safe(&session.session_id));
        let file = write_state_file(&adapter.state_dir, &name, &prompt)?;
        // omp reads an argument naming an existing file as that file.
        opts.args.push("--append-system-prompt".into());
        opts.args.push(file.into());
    }
    if let Some(dir) = &adapter.session_dir {
        opts.args.push("--session-dir".into());
        opts.args.push(dir.into());
    }
    if let Some(mode) = session.approval_mode.as_ref().or(adapter.approval_mode.as_ref()) {
        opts.args.push("--approval-mode".into());
        opts.args.push(mode.into());
    }
    if let Some(model) = &session.model {
        opts.args.push("--model".into());
        opts.args.push(model.into());
    }
    if let Target::Resume(id) = target {
        opts.args.push("--resume".into());
        opts.args.push(id.into());
    }
    if !session.disabled_tools.is_empty() {
        // Host tools (`set_host_tools`) aren't built-ins and stay enabled.
        let tools: Vec<&str> = DEFAULT_TOOLS.iter().copied().filter(|tool| !session.disabled_tools.iter().any(|off| off == tool)).collect();
        opts.args.push(tools_arg(&tools));
    }
    Ok(opts)
}

fn tools_arg(tools: &[&str]) -> OsString {
    if tools.is_empty() { "--no-tools".into() } else { format!("--tools={}", tools.join(",")).into() }
}

/// omp refuses to start when `--tools` names a built-in this session can't
/// have (`find` without a judge model, …). Drops the tools named in that
/// error from `opts`; `false` when `stderr` isn't that error.
pub fn drop_unavailable_tools(opts: &mut SpawnOptions, stderr: &str) -> bool {
    // "Built-in tool unavailable in this session: find." (one) or
    // "Built-in tools unavailable in this session: find, lsp." (several).
    const MARKER: &str = " unavailable in this session: ";
    let Some(line) = stderr.lines().find(|line| line.contains("Built-in tool") && line.contains(MARKER)) else { return false; };
    let list = line[line.find(MARKER).unwrap_or_default() + MARKER.len()..].trim_end_matches('.');
    let unavailable: Vec<&str> = list.split(',').map(str::trim).filter(|tool| !tool.is_empty()).collect();
    let Some(arg) = opts.args.iter_mut().find(|arg| arg.to_str().is_some_and(|arg| arg.starts_with("--tools="))) else { return false; };
    let current = arg.to_string_lossy().into_owned();
    let tools: Vec<&str> = current["--tools=".len()..].split(',').filter(|tool| !unavailable.contains(tool)).collect();
    let next = tools_arg(&tools);
    if next == *arg { return false; }
    *arg = next;
    true
}

fn write_state_file(dir: &Path, name: &str, content: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    // Skip the write when the file already holds the content, so concurrent
    // starts don't rewrite a file another omp is reading.
    if std::fs::read_to_string(&path).is_ok_and(|existing| existing == content) {
        return Ok(path);
    }
    loa_util::fs::write_atomic(&path, content.as_bytes(), None)?;
    Ok(path)
}

fn file_safe(id: &str) -> String {
    let safe: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    if safe.is_empty() { "session".to_owned() } else { safe }
}

/// Splits a `provider/model` selector.
pub fn split_selector(selector: &str) -> Result<(&str, &str)> {
    selector
        .split_once('/')
        .filter(|(p, m)| !p.is_empty() && !m.is_empty())
        .ok_or_else(|| AdapterError::Protocol(format!("model `{selector}` isn't a provider/model selector")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_delivers_instructions_in_both_memory_modes() {
        let dir = tempfile::tempdir().unwrap();
        let session = SessionConfig {
            session_id: "role-test".into(),
            context: Some("Project context".into()),
            instructions: Some("Role instructions".into()),
            ..Default::default()
        };
        for (mode, expected) in [
            (MemoryMode::AppDigests, "Project context\n\nRole instructions"),
            (MemoryMode::OmpMemory, "Role instructions"),
        ] {
            let mut adapter = OmpAdapterConfig::new("owner", dir.path());
            adapter.memory = mode;
            let opts = spawn_options(&adapter, &session, &Target::New).unwrap();
            let positions: Vec<_> = opts.args.iter().enumerate().filter(|(_, arg)| *arg == "--append-system-prompt").map(|(i, _)| i).collect();
            assert_eq!(positions.len(), 1);
            assert_eq!(std::fs::read_to_string(Path::new(&opts.args[positions[0] + 1])).unwrap(), expected);
        }
    }

    #[test]
    fn context_is_only_delivered_in_app_digest_mode() {
        let dir = tempfile::tempdir().unwrap();
        let session = SessionConfig { context: Some("Digest".into()), ..Default::default() };
        let mut adapter = OmpAdapterConfig::new("owner", dir.path());
        let opts = spawn_options(&adapter, &session, &Target::New).unwrap();
        let overlay = opts.args.iter().position(|arg| arg == "--config").unwrap();
        assert_eq!(std::fs::read_to_string(Path::new(&opts.args[overlay + 1])).unwrap(), super::super::MEMORY_OFF);
        let prompt = opts.args.iter().position(|arg| arg == "--append-system-prompt").unwrap();
        assert_eq!(std::fs::read_to_string(Path::new(&opts.args[prompt + 1])).unwrap(), "Digest");

        adapter.memory = MemoryMode::OmpMemory;
        let opts = spawn_options(&adapter, &session, &Target::New).unwrap();
        assert!(!opts.args.iter().any(|arg| arg == "--config" || arg == "--append-system-prompt"));
    }

    #[test]
    fn instructions_without_context_are_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = OmpAdapterConfig::new("owner", dir.path());
        let session = SessionConfig { instructions: Some("Role: DEEP".into()), ..Default::default() };
        let opts = spawn_options(&adapter, &session, &Target::New).unwrap();
        let prompt = opts.args.iter().position(|arg| arg == "--append-system-prompt").unwrap();
        assert_eq!(std::fs::read_to_string(Path::new(&opts.args[prompt + 1])).unwrap(), "Role: DEEP");
    }
}
