//! omp's own tools served to other harnesses over MCP. omp can't run a tool
//! directly, so the bridge runs `omp --mode rpc` against a loopback model it
//! serves itself ([`provider`]): each call becomes a prompt the scripted model
//! answers with exactly that tool call, and the `tool_execution_end` result
//! goes back to the MCP client. No model is ever contacted.

mod provider;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use async_trait::async_trait;
use loa_contracts::harness::{HostToolSpec, McpServerSpec, McpTransport};
use loa_contracts::tools::{CallCtx, DynTool, OutputBlock, ToolAnnotations, ToolOutput, ToolSet};
use loa_harness::omp::rpc::{Command, Event, Events, Incoming, PromptAck, RpcClient, SessionState, SpawnOptions};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::toolkit::ToolSetMcp;
use crate::{Error, Result};
use provider::{ScriptedProvider, call_prompt};

/// omp tools offered by default: hashline reads and edits, AST search and edits, LSP and DAP.
pub const DEFAULT_TOOLS: &[&str] = &["read", "edit", "ast_grep", "ast_edit", "lsp", "debug"];
/// omp tools that change files or run things.
const MUTATING_TOOLS: &[&str] = &["edit", "ast_edit", "debug", "write", "bash"];
/// MCP server name harnesses see; their tool names become `mcp__omp__read`.
pub const SERVER_NAME: &str = "omp";
const INSTRUCTIONS: &str = "omp's own tools. `edit` takes hashline patches: read a file with this \
server's `read` first and copy the `[path#TAG]` header it returns.";
const PROVIDER_ID: &str = "loa-omp-bridge";
const MODEL_ID: &str = "scripted";
const TOKEN_ENV: &str = "LOA_OMP_BRIDGE_TOKEN";
/// Set by omp for processes its tools spawn; the bridge's omp is never part of that session.
const INHERITED_SESSION_ENV: &[&str] = &["PI_SESSION_FILE", "PI_SESSION_ID", "PI_TOOL_CALL_ID", "PI_TERMINAL_OWNER_PID"];
/// Tool output replayed in omp's transcript before the bridge starts a fresh omp session.
const HISTORY_BUDGET_BYTES: usize = 8 << 20;
pub const USAGE: &str = "usage: loa omp-bridge --state-dir DIR [--cwd DIR] [--omp PATH] [--tools a,b,...] [--config FILE]...";

/// How to run omp for the bridge.
#[derive(Debug, Clone)]
pub struct OmpBridgeConfig {
    pub program: PathBuf,
    /// Workspace the tools act on.
    pub cwd: PathBuf,
    /// Owned by the bridge; omp's agent directory lives inside it, never `~/.omp/agent`.
    pub state_dir: PathBuf,
    pub tools: Vec<String>,
    /// Extra `--config` overlays, such as the user's omp config for LSP settings.
    pub config_overlays: Vec<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
}

impl OmpBridgeConfig {
    /// Runs `omp` from `PATH` on `cwd` with [`DEFAULT_TOOLS`].
    pub fn new(cwd: impl Into<PathBuf>, state_dir: impl Into<PathBuf>) -> Self {
        let tools = DEFAULT_TOOLS.iter().map(|tool| (*tool).to_owned()).collect();
        Self { program: "omp".into(), cwd: cwd.into(), state_dir: state_dir.into(), tools, config_overlays: Vec::new(), env: Vec::new() }
    }

    /// Reads `--state-dir DIR [--cwd DIR] [--omp PATH] [--tools a,b] [--config FILE]...`.
    pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let (mut state, mut cwd, mut program, mut tools, mut overlays): (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>, _, _) = (None, None, None, None, Vec::new());
        while let Some(flag) = args.next() {
            let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
            match flag.as_str() {
                "--state-dir" => state = Some(value.into()),
                "--cwd" => cwd = Some(value.into()),
                "--omp" => program = Some(value.into()),
                "--tools" => tools = Some(value.split(',').map(str::to_owned).collect()),
                "--config" => overlays.push(value.into()),
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        let cwd = match cwd {
            Some(cwd) => cwd,
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };
        let mut config = Self::new(cwd, state.ok_or("--state-dir is required")?);
        config.program = program.unwrap_or(config.program);
        config.tools = tools.unwrap_or(config.tools);
        config.config_overlays = overlays;
        Ok(config)
    }

    /// The arguments [`OmpBridgeConfig::parse`] reads back into this config.
    pub fn args(&self) -> Vec<String> {
        let path = |p: &Path| p.to_string_lossy().into_owned();
        let mut out = vec!["--state-dir".into(), path(&self.state_dir), "--cwd".into(), path(&self.cwd)];
        out.extend(["--omp".into(), path(&self.program), "--tools".into(), self.tools.join(",")]);
        for overlay in &self.config_overlays {
            out.extend(["--config".into(), path(overlay)]);
        }
        out
    }

    /// The stdio MCP server running this bridge as `program` with `prefix`
    /// arguments (the app passes its own executable and `["omp-bridge"]`).
    pub fn session_spec(&self, program: &Path, prefix: &[&str]) -> McpServerSpec {
        let args = prefix.iter().map(|arg| (*arg).to_owned()).chain(self.args()).collect();
        let env: BTreeMap<_, _> = self.env.iter().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect();
        McpServerSpec { name: SERVER_NAME.into(), transport: McpTransport::Stdio { command: program.to_string_lossy().into_owned(), args, env } }
    }
}

/// Runs omp's tools in an omp RPC process, one call at a time.
pub struct OmpExecutor {
    session: Arc<Mutex<Session>>,
    tools: Vec<HostToolSpec>,
    _provider: ScriptedProvider,
}

struct Session {
    rpc: RpcClient,
    events: Events,
    calls: u64,
    history_bytes: usize,
}

impl OmpExecutor {
    /// Starts the loopback provider and omp, reading each tool's schema from omp.
    pub async fn start(config: &OmpBridgeConfig) -> Result<Arc<Self>> {
        let (session, provider, dumped) = launch(config, Some(&config.tools)).await?;
        let tools = select_tools(&dumped, &config.tools)?;
        Ok(Arc::new(Self { session: Arc::new(Mutex::new(session)), tools, _provider: provider }))
    }

    /// The executor's tools as a [`ToolSet`].
    pub fn tool_set(self: &Arc<Self>) -> ToolSet {
        let mut set = ToolSet::new();
        for spec in &self.tools {
            set.add(Arc::new(OmpTool { spec: spec.clone(), executor: self.clone() }));
        }
        set
    }

    /// Runs `name`. Dropping the future aborts omp's turn.
    pub async fn call(&self, name: &str, arguments: Map<String, Value>) -> Result<ToolOutput, String> {
        let mut session = self.session.clone().lock_owned().await;
        let cancel = CancellationToken::new();
        let _abort_on_drop = cancel.clone().drop_guard();
        let name = name.to_owned();
        tokio::spawn(async move { session.run(&name, arguments, cancel).await }).await.map_err(|e| e.to_string())?
    }
}

struct OmpTool {
    spec: HostToolSpec,
    executor: Arc<OmpExecutor>,
}

#[async_trait]
impl DynTool for OmpTool {
    fn spec(&self) -> HostToolSpec { self.spec.clone() }
    fn annotations(&self) -> ToolAnnotations {
        ToolAnnotations { destructive: MUTATING_TOOLS.contains(&self.spec.name.as_str()), ..ToolAnnotations::default() }
    }
    async fn call(&self, _ctx: &CallCtx, args: Value) -> Result<ToolOutput, String> {
        let Value::Object(args) = args else { return Err("arguments must be a JSON object".into()) };
        self.executor.call(&self.spec.name, args).await
    }
}

/// Every tool omp enables by default, with its schema, read from an omp run
/// the bridge's way (no model contacted). `config.tools` is ignored.
pub async fn list_omp_tools(config: &OmpBridgeConfig) -> Result<Vec<HostToolSpec>> {
    let (_, _, dumped) = launch(config, None).await?;
    let names: Vec<String> = dumped.iter().filter_map(|tool| tool["name"].as_str().map(str::to_owned)).collect();
    select_tools(&dumped, &names)
}

/// Serves the bridge on stdin and stdout until the client leaves (`loa omp-bridge`).
pub async fn run_cli(args: impl Iterator<Item = String>) -> ExitCode {
    let config = match OmpBridgeConfig::parse(args) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let served = match OmpExecutor::start(&config).await {
        Ok(executor) => ToolSetMcp::new(executor.tool_set(), SERVER_NAME).with_instructions(INSTRUCTIONS).serve_stdio().await,
        Err(error) => Err(error),
    };
    served.map_or_else(|error| { eprintln!("loa omp-bridge: {error}"); ExitCode::FAILURE }, |()| ExitCode::SUCCESS)
}

/// Starts omp against the loopback provider and returns `get_state`'s
/// `dumpTools`. `tools` limits omp to those tools.
async fn launch(config: &OmpBridgeConfig, tools: Option<&[String]>) -> Result<(Session, ScriptedProvider, Vec<Value>)> {
    let io = |e: std::io::Error| Error::Bridge(e.to_string());
    let token = loa_util::text::random_hex(16);
    let provider = ScriptedProvider::start(&token).await.map_err(io)?;
    let agent_dir = config.state_dir.join("agent");
    tokio::fs::create_dir_all(&agent_dir).await.map_err(io)?;
    let model = json!({"id": MODEL_ID, "name": "omp tools bridge (no model)", "reasoning": false, "input": ["text"],
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}, "contextWindow": 1_000_000, "maxTokens": 4096});
    let models = json!({"providers": {PROVIDER_ID: {"baseUrl": provider.base_url(), "apiKey": TOKEN_ENV, "api": "openai-completions", "models": [model]}}});
    // JSON is valid YAML, so serde_json writes omp's YAML config files.
    tokio::fs::write(agent_dir.join("models.yml"), models.to_string()).await.map_err(io)?;
    let overlay = config.state_dir.join("bridge-config.yml");
    tokio::fs::write(&overlay, json!({"astGrep": {"enabled": true}, "tools": {"xdev": false}}).to_string()).await.map_err(io)?;

    let mut options = SpawnOptions::new(&config.program);
    options.no_ui = true;
    let flags = ["--no-session", "--no-extensions", "--no-skills", "--no-rules", "--no-context-files", "--no-title", "--offline", "--auto-approve"];
    options.args = flags.iter().map(OsString::from).collect();
    options.args.extend(["--model".into(), format!("{PROVIDER_ID}/{MODEL_ID}").into(), "--config".into(), overlay.into()]);
    for extra in &config.config_overlays {
        options.args.extend(["--config".into(), extra.into()]);
    }
    if let Some(tools) = tools {
        options.args.extend(["--tools".into(), tools.join(",").into()]);
    }
    options.cwd = Some(config.cwd.clone());
    options.env = vec![("PI_CODING_AGENT_DIR".into(), agent_dir.into()), (TOKEN_ENV.into(), token.into())];
    options.env.extend(config.env.iter().cloned());
    options.env_remove = INHERITED_SESSION_ENV.iter().map(OsString::from).collect();
    let rpc_error = |e: loa_harness::omp::rpc::RpcError| Error::Bridge(e.to_string());
    let (rpc, events) = RpcClient::spawn(options).await.map_err(rpc_error)?;
    let mut state: SessionState = rpc.request(Command::GetState).await.map_err(rpc_error)?;
    let Some(Value::Array(dumped)) = state.extra.remove("dumpTools") else { return Err(Error::Bridge("get_state has no dumpTools".into())) };
    Ok((Session { rpc, events, calls: 0, history_bytes: 0 }, provider, dumped))
}

impl Session {
    /// Runs one tool through a prompt the loopback provider answers with that call.
    async fn run(&mut self, name: &str, arguments: Map<String, Value>, cancel: CancellationToken) -> Result<ToolOutput, String> {
        if self.history_bytes > HISTORY_BUDGET_BYTES {
            self.rpc.request::<Value>(Command::NewSession).await.map_err(|e| e.to_string())?;
            self.history_bytes = 0;
        }
        self.calls += 1;
        let (call_id, id) = (format!("loa-omp-bridge-call-{}", self.calls), self.rpc.reserve_id());
        let (rpc, message, prompt_id) = (self.rpc.clone(), call_prompt(&call_id, name, arguments), id.clone());
        // Spawned so the prompt is sent even if this future is dropped.
        let mut ack = tokio::spawn(async move { rpc.request_as::<PromptAck>(prompt_id, Command::Prompt { message, images: Vec::new(), streaming_behavior: None }).await });
        let (mut acked, mut aborted, mut output, mut rejection) = (false, false, None, None);
        let not_run = |reason: String| format!("omp ended the turn without running `{name}`: {reason}");
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled(), if !aborted => {
                    aborted = true;
                    self.rpc.request::<Value>(Command::Abort).await.map_err(|e| e.to_string())?;
                }
                joined = &mut ack, if !acked => {
                    acked = true;
                    match joined.map_err(|e| e.to_string())? {
                        Err(e) => rejection = Some(e.to_string()),
                        Ok(PromptAck { agent_invoked: Some(false) }) => {
                            return output.ok_or_else(|| not_run("omp handled the prompt without an agent turn".into()));
                        }
                        Ok(_) => {}
                    }
                }
                incoming = self.events.recv() => match incoming {
                    Some(Incoming::Event(Event::ToolExecutionEnd { tool_call_id, result, is_error, .. })) if tool_call_id == call_id => {
                        let out = tool_output(result.as_ref(), is_error);
                        self.history_bytes += out.blocks.iter().map(|b| match b { OutputBlock::Text(t) => t.len(), OutputBlock::Image { data, .. } => data.len() }).sum::<usize>();
                        output = Some(out);
                    }
                    Some(Incoming::Event(Event::PromptResult { id: Some(done), status, error, .. })) if done == id => {
                        if aborted {
                            return Err("tool call cancelled".into());
                        }
                        let status = format!("turn {}", format!("{status:?}").to_lowercase());
                        let reason = error.map_or_else(|| status.clone(), |e| format!("{status}: {}", e.message));
                        return output.ok_or_else(|| rejection.unwrap_or_else(|| not_run(reason)));
                    }
                    Some(Incoming::Exited { stderr, .. }) => return Err(format!("omp exited: {stderr}")),
                    None => return Err("omp exited".into()),
                    _ => {}
                },
            }
        }
    }
}

/// Picks the wanted tools out of `dumpTools`, keeping omp's schemas, in the given order.
fn select_tools(dumped: &[Value], wanted: &[String]) -> Result<Vec<HostToolSpec>> {
    let bridge = |message: String| Error::Bridge(message);
    wanted
        .iter()
        .map(|name| {
            let tool = dumped.iter().find(|tool| tool["name"] == name.as_str()).ok_or_else(|| bridge(format!("unknown tool `{name}`")))?;
            let schema = tool.get("parameters").filter(|p| p.is_object()).ok_or_else(|| bridge(format!("omp reported no schema for `{name}`")))?;
            let description = tool["description"].as_str().unwrap_or_default().to_owned();
            Ok(HostToolSpec { name: name.clone(), description, input_schema: schema.to_string() })
        })
        .collect()
}

/// The result of a `tool_execution_end` frame.
fn tool_output(result: Option<&Value>, is_error: bool) -> ToolOutput {
    let blocks = result.and_then(|r| r["content"].as_array()).map(Vec::as_slice).unwrap_or_default();
    let blocks = blocks.iter().map(|block| {
        let image = (block["type"] == "image").then(|| crate::toolkit::image(block["data"].as_str()?, block["mimeType"].as_str()?)).flatten();
        image.unwrap_or_else(|| OutputBlock::Text(block["text"].as_str().map_or_else(|| block.to_string(), str::to_owned)))
    });
    ToolOutput { blocks: blocks.collect(), is_error }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_round_trip_and_spec_runs_the_app_subcommand() {
        let mut config = OmpBridgeConfig::new("/work", "/state");
        (config.program, config.tools, config.config_overlays) = ("/opt/omp".into(), vec!["read".into(), "edit".into()], vec!["/c.yml".into()]);
        let McpTransport::Stdio { command, args, .. } = config.session_spec(Path::new("/usr/bin/loa-app"), &["omp-bridge"]).transport else { panic!("stdio") };
        assert_eq!((command.as_str(), args[0].as_str()), ("/usr/bin/loa-app", "omp-bridge"));
        let parsed = OmpBridgeConfig::parse(args.into_iter().skip(1)).unwrap();
        assert_eq!((parsed.cwd, parsed.state_dir, parsed.program), (config.cwd, config.state_dir, config.program));
        assert_eq!((parsed.tools, parsed.config_overlays), (config.tools, config.config_overlays));
        assert!(OmpBridgeConfig::parse(["--cwd".to_owned()].into_iter()).is_err());
        assert!(OmpBridgeConfig::parse(["--bogus".to_owned(), "x".to_owned()].into_iter()).is_err());
    }

    #[test]
    fn schemas_and_results_come_from_omp_frames() {
        let dumped = [
            json!({"name": "read", "description": "Read files", "parameters": {"type": "object"}}),
            json!({"name": "edit", "description": "Hashline patches", "parameters": {"type": "object", "properties": {"input": {}}}}),
        ];
        let tools = select_tools(&dumped, &["edit".to_owned(), "read".to_owned()]).unwrap();
        assert_eq!((tools[0].name.as_str(), tools[1].description.as_str()), ("edit", "Read files"));
        assert!(tools[0].input_schema.contains("input"));
        assert!(select_tools(&dumped, &["lsp".to_owned()]).is_err());
        let result = json!({"content": [{"type": "text", "text": "stale tag"}, {"type": "image", "data": "aGk=", "mimeType": "image/png"}]});
        let out = tool_output(Some(&result), true);
        assert!(out.is_error);
        assert_eq!(out.blocks, [OutputBlock::Text("stale tag".into()), OutputBlock::Image { mime: "image/png".into(), data: "hi".into() }]);
    }
}
