//! [`ToolExecutor`] backed by a real `omp --mode rpc` process.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::provider::{ScriptedProvider, call_prompt};
use crate::rpc::RpcChild;
use crate::{Error, OutputBlock, Result, ToolExecutor, ToolOutput, ToolSpec};

/// omp tools the bridge offers by default: hashline reads and edits, AST search
/// and edits, LSP and DAP.
pub const DEFAULT_TOOLS: &[&str] = &["read", "edit", "ast_grep", "ast_edit", "lsp", "debug"];

/// Provider and model ids the bridge registers in its own omp agent directory.
const PROVIDER_ID: &str = "loa-omp-bridge";
const MODEL_ID: &str = "scripted";

/// Environment variable that carries the loopback provider's bearer token to omp.
const TOKEN_ENV: &str = "LOA_OMP_BRIDGE_TOKEN";

/// Variables omp sets for processes its tools spawn.
const INHERITED_SESSION_ENV: &[&str] = &[
    "PI_SESSION_FILE",
    "PI_SESSION_ID",
    "PI_TOOL_CALL_ID",
    "PI_TERMINAL_OWNER_PID",
];

/// Tool output text accumulated in omp's transcript before the bridge starts a
/// fresh omp session, so the transcript replayed on every call stays bounded.
const HISTORY_BUDGET_BYTES: usize = 8 << 20;

/// How to run omp for the bridge.
#[derive(Debug, Clone)]
pub struct OmpBridgeConfig {
    /// The omp executable.
    pub program: PathBuf,
    /// Workspace the tools act on (omp's working directory).
    pub cwd: PathBuf,
    /// Directory the bridge owns. omp's agent directory (`PI_CODING_AGENT_DIR`)
    /// lives inside it, so the bridge never touches the user's `~/.omp/agent`.
    pub state_dir: PathBuf,
    /// omp tool names to offer, in the order MCP clients see them.
    pub tools: Vec<String>,
    /// Extra `config.yml` style files passed to omp as `--config` overlays, for
    /// example the user's own omp config so LSP and debugger settings apply.
    pub config_overlays: Vec<PathBuf>,
    /// Extra environment variables for the omp process.
    pub env: Vec<(OsString, OsString)>,
}

impl OmpBridgeConfig {
    /// Runs `omp` from `PATH` on `cwd` with [`DEFAULT_TOOLS`].
    pub fn new(cwd: impl Into<PathBuf>, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            program: PathBuf::from("omp"),
            cwd: cwd.into(),
            state_dir: state_dir.into(),
            tools: DEFAULT_TOOLS.iter().map(|tool| (*tool).to_owned()).collect(),
            config_overlays: Vec::new(),
            env: Vec::new(),
        }
    }
}

/// Runs omp's own tools in an omp RPC process, with no model involved.
pub struct OmpExecutor {
    tools: Vec<ToolSpec>,
    session: Mutex<Session>,
    _provider: ScriptedProvider,
}

struct Session {
    rpc: RpcChild,
    next_id: u64,
    /// Prompt whose `prompt_result` has not been read yet (its caller was dropped).
    in_flight: Option<String>,
    history_bytes: usize,
}

impl OmpExecutor {
    /// Starts the loopback provider and omp, and reads each tool's JSON Schema from omp.
    pub async fn start(config: OmpBridgeConfig) -> Result<Self> {
        let (session, provider, state) = launch(&config, Some(&config.tools)).await?;
        let tools = select_tools(&state, &config.tools)?;
        Ok(Self {
            tools,
            session: Mutex::new(session),
            _provider: provider,
        })
    }
}

/// Every tool omp enables by default (its `tools` settings, no extensions or
/// skills), with descriptions and schemas, as omp reports them. Runs omp the
/// bridge's way, so no model is contacted, and stops it again. `config.tools`
/// is ignored.
pub async fn list_omp_tools(config: &OmpBridgeConfig) -> Result<Vec<ToolSpec>> {
    let (session, _provider, state) = launch(config, None).await?;
    drop(session);
    let names: Vec<String> = dumped_tools(&state)?
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_owned))
        .collect();
    select_tools(&state, &names)
}

/// Starts omp against the loopback provider and reads `get_state`. `tools`
/// limits omp to those tools; `None` keeps omp's defaults.
async fn launch(
    config: &OmpBridgeConfig,
    tools: Option<&[String]>,
) -> Result<(Session, ScriptedProvider, Value)> {
    let token = random_token()?;
    let provider = ScriptedProvider::start(&token).await?;
    let agent_dir = config.state_dir.join("agent");
    tokio::fs::create_dir_all(&agent_dir).await?;
    write_models(&agent_dir, &provider.base_url()).await?;
    let overlay = config.state_dir.join("bridge-config.yml");
    // JSON is valid YAML, so serde_json writes omp's YAML config files.
    let settings = json!({"astGrep": {"enabled": true}, "tools": {"xdev": false}});
    tokio::fs::write(&overlay, serde_json::to_vec_pretty(&settings)?).await?;

    let mut command = Command::new(&config.program);
    command
        .args([
            "--mode",
            "rpc",
            "--no-session",
            "--no-ui",
            "--no-extensions",
            "--no-skills",
            "--no-rules",
            "--no-context-files",
            "--no-title",
            "--offline",
            "--auto-approve",
            "--model",
        ])
        .arg(format!("{PROVIDER_ID}/{MODEL_ID}"))
        .arg("--config")
        .arg(&overlay);
    for extra in &config.config_overlays {
        command.arg("--config").arg(extra);
    }
    if let Some(tools) = tools {
        command.arg("--tools").arg(tools.join(","));
    }
    command
        .current_dir(&config.cwd)
        .env("PI_CODING_AGENT_DIR", &agent_dir)
        .env(TOKEN_ENV, &token);
    // When the bridge itself runs under an omp tool call, these name that
    // call's session; the bridge's omp is never part of it.
    for inherited in INHERITED_SESSION_ENV {
        command.env_remove(inherited);
    }
    command.envs(config.env.iter().map(|(key, value)| (key, value)));

    let mut session = Session {
        rpc: RpcChild::spawn(command)?,
        next_id: 0,
        in_flight: None,
        history_bytes: 0,
    };
    session.handshake().await?;
    let state = session.command(json!({"type": "get_state"})).await?;
    Ok((session, provider, state))
}

#[async_trait::async_trait]
impl ToolExecutor for OmpExecutor {
    fn tools(&self) -> &[ToolSpec] {
        &self.tools
    }

    async fn call(
        &self,
        name: &str,
        arguments: Map<String, Value>,
        cancel: CancellationToken,
    ) -> Result<ToolOutput> {
        if !self.tools.iter().any(|tool| tool.name == name) {
            return Err(Error::UnknownTool(name.to_owned()));
        }
        let mut session = self.session.lock().await;
        session.settle().await?;
        if session.history_bytes > HISTORY_BUDGET_BYTES {
            session.command(json!({"type": "new_session"})).await?;
            session.history_bytes = 0;
        }
        session.run_tool(name, &arguments, &cancel).await
    }
}

impl Session {
    fn next_id(&mut self, kind: &str) -> String {
        self.next_id += 1;
        format!("loa-omp-bridge-{kind}-{}", self.next_id)
    }

    /// Waits for omp's ready frame and switches to protocol v2 when offered.
    async fn handshake(&mut self) -> Result<()> {
        let ready = loop {
            let frame = self.rpc.next().await?;
            if frame_type(&frame) == Some("ready") {
                break frame;
            }
        };
        if let Some(max) = ready
            .get("maxReassembledFrameBytes")
            .and_then(Value::as_u64)
            .and_then(|max| usize::try_from(max).ok())
        {
            self.rpc.set_max_reassembled_bytes(max);
        }
        let offers_v2 = ready
            .get("supportedProtocolVersions")
            .and_then(Value::as_array)
            .is_some_and(|versions| versions.iter().any(|version| version.as_u64() == Some(2)));
        if offers_v2 {
            self.command(json!({"type": "negotiate_protocol", "protocolVersion": 2}))
                .await?;
        }
        Ok(())
    }

    /// Sends one command and returns its response `data`.
    async fn command(&mut self, mut command: Value) -> Result<Value> {
        let id = self.next_id("cmd");
        command["id"] = Value::String(id.clone());
        self.rpc.send(&command).await?;
        loop {
            let frame = self.rpc.next().await?;
            if frame_type(&frame) == Some("response") && frame_id(&frame) == Some(&id) {
                return response_data(frame);
            }
        }
    }

    /// Finishes a prompt a dropped caller left running: aborts it and reads up to its result.
    async fn settle(&mut self) -> Result<()> {
        let Some(id) = self.in_flight.clone() else {
            return Ok(());
        };
        self.rpc.send(&json!({"type": "abort"})).await?;
        loop {
            let frame = self.rpc.next().await?;
            if prompt_finished(&frame, &id) {
                self.in_flight = None;
                return Ok(());
            }
        }
    }

    /// Runs one tool through a prompt the loopback provider answers with that call.
    async fn run_tool(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
        cancel: &CancellationToken,
    ) -> Result<ToolOutput> {
        let request_id = self.next_id("prompt");
        let call_id = self.next_id("call");
        let message = call_prompt(&call_id, name, arguments)?;
        self.rpc
            .send(&json!({"id": request_id, "type": "prompt", "message": message}))
            .await?;
        self.in_flight = Some(request_id.clone());

        let mut output = None;
        let mut rejection = None;
        loop {
            let frame = tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    self.settle().await?;
                    return Err(Error::Cancelled);
                }
                frame = self.rpc.next() => frame?,
            };
            match frame_type(&frame) {
                Some("tool_execution_end")
                    if frame.get("toolCallId").and_then(Value::as_str) == Some(&call_id) =>
                {
                    let tool_output = tool_output(&frame);
                    self.history_bytes += tool_output
                        .content
                        .iter()
                        .map(|block| match block {
                            OutputBlock::Text(text) => text.len(),
                            OutputBlock::Image { data, .. } => data.len(),
                        })
                        .sum::<usize>();
                    output = Some(tool_output);
                }
                Some("response") if frame_id(&frame) == Some(&request_id) => {
                    let finished_locally = agent_not_invoked(&frame);
                    // A refused prompt is still completed by a `prompt_result`.
                    if let Err(error) = response_data(frame) {
                        rejection = Some(error);
                    } else if finished_locally {
                        self.in_flight = None;
                        return output.ok_or_else(|| Error::NotRun {
                            tool: name.to_owned(),
                            reason: "omp handled the prompt without an agent turn".into(),
                        });
                    }
                }
                Some("prompt_result") if frame_id(&frame) == Some(&request_id) => {
                    self.in_flight = None;
                    if let Some(output) = output {
                        return Ok(output);
                    }
                    return Err(rejection.unwrap_or_else(|| Error::NotRun {
                        tool: name.to_owned(),
                        reason: prompt_failure(&frame),
                    }));
                }
                _ => {}
            }
        }
    }
}

/// `get_state`'s `dumpTools`: every active tool with its schema.
fn dumped_tools(state: &Value) -> Result<&Vec<Value>> {
    state
        .get("dumpTools")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Frame("get_state has no dumpTools".into()))
}

/// Picks the configured tools out of `get_state`'s `dumpTools`, keeping omp's schemas.
fn select_tools(state: &Value, wanted: &[String]) -> Result<Vec<ToolSpec>> {
    let dumped = dumped_tools(state)?;
    wanted
        .iter()
        .map(|name| {
            let tool = dumped
                .iter()
                .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
                .ok_or_else(|| Error::UnknownTool(name.clone()))?;
            let input_schema = match tool.get("parameters") {
                Some(Value::Object(schema)) => schema.clone(),
                _ => return Err(Error::Frame(format!("omp reported no schema for `{name}`"))),
            };
            Ok(ToolSpec {
                name: name.clone(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                input_schema,
            })
        })
        .collect()
}

/// Converts a `tool_execution_end` frame into the tool's output.
fn tool_output(frame: &Value) -> ToolOutput {
    let content = frame
        .get("result")
        .and_then(|result| result.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .map(|block| {
                    let field = |name| block.get(name).and_then(Value::as_str);
                    match (field("type"), field("text"), field("data"), field("mimeType")) {
                        (Some("text"), Some(text), _, _) => OutputBlock::Text(text.to_owned()),
                        (Some("image"), _, Some(data), Some(mime_type)) => OutputBlock::Image {
                            data: data.to_owned(),
                            mime_type: mime_type.to_owned(),
                        },
                        _ => OutputBlock::Text(block.to_string()),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    ToolOutput {
        content,
        is_error: frame.get("isError").and_then(Value::as_bool).unwrap_or(false),
    }
}

fn prompt_finished(frame: &Value, id: &str) -> bool {
    match frame_type(frame) {
        Some("prompt_result") => frame_id(frame) == Some(id),
        Some("response") => frame_id(frame) == Some(id) && agent_not_invoked(frame),
        _ => false,
    }
}

/// A successful prompt response that says no agent turn will follow.
fn agent_not_invoked(frame: &Value) -> bool {
    frame.get("success").and_then(Value::as_bool) == Some(true)
        && frame
            .get("data")
            .and_then(|data| data.get("agentInvoked"))
            .and_then(Value::as_bool)
            == Some(false)
}

fn prompt_failure(frame: &Value) -> String {
    let status = frame.get("status").and_then(Value::as_str).unwrap_or("unknown");
    match frame
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        Some(message) => format!("turn {status}: {message}"),
        None => format!("turn {status}"),
    }
}

fn response_data(frame: Value) -> Result<Value> {
    let Value::Object(mut frame) = frame else {
        return Err(Error::Frame("response is not an object".into()));
    };
    if frame.get("success").and_then(Value::as_bool) == Some(true) {
        return Ok(frame.remove("data").unwrap_or(Value::Null));
    }
    let text = |value: Option<Value>| match value {
        Some(Value::String(text)) => text,
        Some(other) => other.to_string(),
        None => String::new(),
    };
    Err(Error::Rejected {
        command: text(frame.remove("command")),
        message: text(frame.remove("error")),
    })
}

fn frame_type(frame: &Value) -> Option<&str> {
    frame.get("type").and_then(Value::as_str)
}

fn frame_id(frame: &Value) -> Option<&str> {
    frame.get("id").and_then(Value::as_str)
}

/// Writes `models.yml` registering the loopback provider as omp's only model.
async fn write_models(agent_dir: &Path, base_url: &str) -> Result<()> {
    let models = json!({
        "providers": {
            PROVIDER_ID: {
                "baseUrl": base_url,
                "apiKey": TOKEN_ENV,
                "api": "openai-completions",
                "models": [{
                    "id": MODEL_ID,
                    "name": "omp tools bridge (no model)",
                    "reasoning": false,
                    "input": ["text"],
                    "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                    "contextWindow": 1_000_000,
                    "maxTokens": 4096,
                }],
            },
        },
    });
    tokio::fs::write(agent_dir.join("models.yml"), serde_json::to_vec_pretty(&models)?).await?;
    Ok(())
}

fn random_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| Error::Io(std::io::Error::other(error.to_string())))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_come_from_omp_in_configured_order() {
        let state = json!({"dumpTools": [
            {"name": "read", "description": "Read files", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}},
            {"name": "edit", "description": "Hashline patches", "parameters": {"type": "object", "properties": {"input": {"type": "string"}}}},
        ]});
        let tools = select_tools(&state, &["edit".to_owned(), "read".to_owned()]).unwrap();
        assert_eq!(tools[0].name, "edit");
        assert_eq!(tools[0].input_schema["properties"]["input"]["type"], "string");
        assert_eq!(tools[1].description, "Read files");
        assert!(matches!(
            select_tools(&state, &["lsp".to_owned()]),
            Err(Error::UnknownTool(name)) if name == "lsp"
        ));
    }

    #[test]
    fn tool_errors_and_images_survive_conversion() {
        let frame = json!({
            "type": "tool_execution_end",
            "toolCallId": "c",
            "isError": true,
            "result": {"content": [
                {"type": "text", "text": "stale tag"},
                {"type": "image", "data": "aGk=", "mimeType": "image/png"},
            ]},
        });
        let output = tool_output(&frame);
        assert!(output.is_error);
        assert_eq!(
            output.content,
            [
                OutputBlock::Text("stale tag".into()),
                OutputBlock::Image { data: "aGk=".into(), mime_type: "image/png".into() },
            ]
        );
    }
}
