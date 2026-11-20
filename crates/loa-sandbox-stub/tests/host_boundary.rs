use async_trait::async_trait;
use loa_adapter::{Adapter, AdapterError, Answer, Capabilities, HarnessSession, HostTools, Prompt, SessionConfig, SessionState};
use loa_core::{Dom, NodeId, PatchSink, SessionMode};
use loa_sandbox::provider::local::LocalProvider;
use loa_sandbox::tool::{SandboxTask, SandboxTaskArgs};
use loa_sandbox::worker::MemoryJournal;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The `sandbox_bash` call that prints the host key, or `unset` without it.
#[cfg(unix)]
const PRINT_HOST_KEY: &str = r#"{"command":"printf '%s' \"${HOST_API_KEY-unset}\""}"#;
#[cfg(windows)]
const PRINT_HOST_KEY: &str = r#"{"command":"if defined HOST_API_KEY (echo %HOST_API_KEY%) else (echo unset)"}"#;

struct TestAdapter {
    observed: Arc<Mutex<Vec<String>>>,
}

struct TestSession {
    tools: Arc<dyn HostTools>,
    observed: Arc<Mutex<Vec<String>>>,
    id: String,
}

#[async_trait]
impl Adapter for TestAdapter {
    fn harness(&self) -> &'static str { "test" }
    fn capabilities(&self) -> Capabilities { Capabilities::default() }
    async fn start(&self, config: SessionConfig, _sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        assert_eq!(config.env.get("HOST_API_KEY").map(String::as_str), Some("host-only-value"));
        assert!(config.mcp_servers.is_empty());
        assert!(config.disabled_tools.iter().any(|name| name == "bash"));
        let tools = config.host_tool_handler.ok_or_else(|| AdapterError::Format("worker has no sandbox tools".into()))?;
        Ok(Box::new(TestSession { tools, observed: Arc::clone(&self.observed), id: config.session_id }))
    }
    async fn resume(&self, _: SessionConfig, _: &Dom, _: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        Err(AdapterError::Format("unexpected resume".into()))
    }
}

#[async_trait]
impl HarnessSession for TestSession {
    async fn send(&self, _: Prompt) -> loa_adapter::Result<()> {
        let env = self.tools.call(&self.id, "sandbox_bash", PRINT_HOST_KEY)
            .await.map_err(AdapterError::Protocol)?;
        let files = self.tools.call(&self.id, "sandbox_list", r#"{"path":"."}"#)
            .await.map_err(AdapterError::Protocol)?;
        let read = self.tools.call(&self.id, "sandbox_read", r#"{"path":"public.txt"}"#)
            .await.map_err(AdapterError::Protocol)?;
        self.observed.lock().unwrap().extend([env, files, read]);
        Ok(())
    }
    async fn interrupt(&self) -> loa_adapter::Result<()> { Ok(()) }
    async fn answer(&self, _: NodeId, _: Answer) -> loa_adapter::Result<()> { Ok(()) }
    async fn set_mode(&self, _: SessionMode) -> loa_adapter::Result<()> { Ok(()) }
    fn harness(&self) -> &'static str { "test" }
    fn state(&self) -> SessionState { SessionState::Idle }
    async fn wait_idle(&self) -> loa_adapter::Result<()> { Ok(()) }
    async fn shutdown(&self) -> loa_adapter::Result<()> { Ok(()) }
}

#[tokio::test]
async fn worker_secrets_stay_on_host_and_only_workspace_files_reach_stub() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("public.txt"), "safe-workspace-content").unwrap();
    let secret = tempfile::tempdir().unwrap();
    std::fs::write(secret.path().join("host-secret.txt"), "host-only-value").unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let mut template = SessionConfig { session_id: "parent".into(), ..SessionConfig::default() };
    template.env.insert("HOST_API_KEY".into(), "host-only-value".into());
    template.cwd = PathBuf::from(secret.path());
    let mut tool = SandboxTask::new(
        Arc::new(TestAdapter { observed: Arc::clone(&observed) }),
        template,
        Arc::new(MemoryJournal::default()),
        temp.path().to_path_buf(),
    );
    tool.register(Arc::new(LocalProvider::new(env!("CARGO_BIN_EXE_loa-sandbox-stub"))));
    let args: SandboxTaskArgs = serde_json::from_str(r#"{"task":"Inspect files","kind":"local"}"#).unwrap();
    tool.execute(args).await.unwrap();
    let observed = observed.lock().unwrap();
    assert!(observed[0].contains("unset"), "sandbox inherited host environment: {}", observed[0]);
    assert!(!observed[1].contains("host-secret.txt"), "sandbox listed host secret: {}", observed[1]);
    assert_eq!(observed[2], "safe-workspace-content");
}
