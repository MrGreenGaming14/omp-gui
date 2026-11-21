//! How an app managed omp process is started: the memory switch, project
//! context delivery and session selection, as CLI arguments.

use loa_adapter::{AdapterError, Result, SessionConfig};
use loa_omp_rpc::{Mode, SpawnOptions};
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

    /// Like [`OmpAdapterConfig::new`], with `state_dir` under the OS temp
    /// directory (`$TMPDIR`).
    pub fn with_temp_state_dir(node: impl Into<String>) -> Self {
        Self::new(node, std::env::temp_dir().join("loa-adapter-omp"))
    }
}

/// The overlay that switches omp's memory backend off for one process.
pub const MEMORY_OFF_OVERLAY: &str = "memory:\n  backend: \"off\"\n";

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
            let overlay = write_state_file(&adapter.state_dir, "memory-off.yml", MEMORY_OFF_OVERLAY)?;
            opts.args.push("--config".into());
            opts.args.push(overlay.into());
            if let Some(context) = session.context.as_deref().filter(|c| !c.trim().is_empty()) {
                let name = format!("context-{}.md", file_safe(&session.session_id));
                let file = write_state_file(&adapter.state_dir, &name, context)?;
                // A file path, never the text: omp reads an argument that
                // names an existing file as that file.
                opts.args.push("--append-system-prompt".into());
                opts.args.push(file.into());
            }
        }
        MemoryMode::OmpMemory => {}
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

    fn args(opts: &SpawnOptions) -> Vec<String> {
        opts.args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
    }

    #[test]
    fn app_digests_turn_omp_memory_off_and_deliver_context_once() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = OmpAdapterConfig::new("node", dir.path());
        let session = SessionConfig {
            session_id: "s/1".into(),
            context: Some("Digest: picked iced".into()),
            ..Default::default()
        };
        let opts = spawn_options(&adapter, &session, &Target::New).unwrap();
        let args = args(&opts);
        let overlay = flag(&args, "--config").expect("memory overlay");
        assert_eq!(std::fs::read_to_string(overlay).unwrap(), MEMORY_OFF_OVERLAY);
        let context = flag(&args, "--append-system-prompt").expect("context file");
        assert_eq!(std::fs::read_to_string(context).unwrap(), "Digest: picked iced");
        assert_eq!(args.iter().filter(|a| *a == "--append-system-prompt").count(), 1);
    }

    #[test]
    fn omp_memory_mode_leaves_memory_on_and_never_injects_app_context() {
        let dir = tempfile::tempdir().unwrap();
        let mut adapter = OmpAdapterConfig::new("node", dir.path());
        adapter.memory = MemoryMode::OmpMemory;
        let session = SessionConfig { context: Some("Digest".into()), ..Default::default() };
        let args = args(&spawn_options(&adapter, &session, &Target::New).unwrap());
        assert!(flag(&args, "--config").is_none(), "user's memory backend stays as configured");
        assert!(flag(&args, "--append-system-prompt").is_none(), "no second summary from the app");
    }

    #[test]
    fn resume_and_model_are_passed_through() {
        let dir = tempfile::tempdir().unwrap();
        let mut adapter = OmpAdapterConfig::new("node", dir.path());
        adapter.session_dir = Some(dir.path().join("sessions"));
        let session = SessionConfig { model: Some("anthropic/claude".into()), ..Default::default() };
        let args = args(&spawn_options(&adapter, &session, &Target::Resume("0190abc".into())).unwrap());
        assert_eq!(flag(&args, "--resume"), Some("0190abc"));
        assert_eq!(flag(&args, "--model"), Some("anthropic/claude"));
        assert!(flag(&args, "--session-dir").is_some());
        assert!(flag(&args, "--approval-mode").is_none(), "omp's configured approval mode stays");
    }

    #[test]
    fn a_sessions_approval_mode_wins_over_the_adapters() {
        let dir = tempfile::tempdir().unwrap();
        let mut adapter = OmpAdapterConfig::new("node", dir.path());
        adapter.approval_mode = Some("always-ask".into());
        let session = SessionConfig { approval_mode: Some("yolo".into()), ..SessionConfig::default() };
        let args = args(&spawn_options(&adapter, &session, &Target::New).unwrap());
        assert_eq!(flag(&args, "--approval-mode"), Some("yolo"));
    }

    #[test]
    fn approval_mode_is_passed_to_omp() {
        let dir = tempfile::tempdir().unwrap();
        let mut adapter = OmpAdapterConfig::new("node", dir.path());
        adapter.approval_mode = Some("always-ask".into());
        let args = args(&spawn_options(&adapter, &SessionConfig::default(), &Target::New).unwrap());
        assert_eq!(flag(&args, "--approval-mode"), Some("always-ask"));
    }

    #[test]
    fn disabled_tools_leave_the_other_built_ins_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = OmpAdapterConfig::new("node", dir.path());
        let none = args_of(&adapter, &SessionConfig::default());
        assert!(!none.iter().any(|a| a.starts_with("--tools") || a == "--no-tools"), "omp's configured tools stay");
        let session = SessionConfig { disabled_tools: vec!["bash".into(), "eval".into()], ..Default::default() };
        let args = args_of(&adapter, &session);
        let tools = args.iter().find_map(|a| a.strip_prefix("--tools=")).expect("a --tools list");
        let tools: Vec<&str> = tools.split(',').collect();
        assert!(!tools.contains(&"bash") && !tools.contains(&"eval"));
        assert_eq!(tools.len(), DEFAULT_TOOLS.len() - 2);
        let all = SessionConfig { disabled_tools: DEFAULT_TOOLS.iter().map(|t| (*t).to_owned()).collect(), ..Default::default() };
        assert!(args_of(&adapter, &all).contains(&"--no-tools".to_owned()));
    }

    fn args_of(adapter: &OmpAdapterConfig, session: &SessionConfig) -> Vec<String> {
        args(&spawn_options(adapter, session, &Target::New).unwrap())
    }

    #[test]
    fn tools_omp_reports_unavailable_are_dropped_for_the_retry() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = OmpAdapterConfig::new("node", dir.path());
        let session = SessionConfig { disabled_tools: vec!["bash".into()], ..Default::default() };
        let mut opts = spawn_options(&adapter, &session, &Target::New).unwrap();
        // omp 19's messages when `find` has no judge model, and for several tools.
        let single = "Error: Built-in tool unavailable in this session: find.\nRun `omp --help` for available flags.";
        assert!(drop_unavailable_tools(&mut opts, single));
        let stderr = "Error: Built-in tools unavailable in this session: lsp, debug.\nRun `omp --help` for available flags.";
        assert!(drop_unavailable_tools(&mut opts, stderr));
        let tools = args(&opts).into_iter().find_map(|a| a.strip_prefix("--tools=").map(str::to_owned)).unwrap();
        assert!(!tools.split(',').any(|t| ["find", "lsp", "debug", "bash"].contains(&t)));
        assert!(tools.split(',').any(|t| t == "read"));
        assert!(!drop_unavailable_tools(&mut opts, stderr), "nothing left to drop");
        assert!(!drop_unavailable_tools(&mut opts, "Error: something else"));
    }
}
