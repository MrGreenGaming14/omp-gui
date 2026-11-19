//! The adapter against a scripted ACP agent on in-memory pipes. Each test
//! plays the agent side by hand: it reads what the client sends, checks it,
//! and answers the way an ACP agent would.

use loa_adapter::{
    Adapter, AdapterError, Answer, Attachment, Feature, HarnessSession, McpServerSpec, McpTransport, Prompt, Result,
    SessionConfig, SessionState,
};
use loa_adapter_acp::{AcpAdapter, Connector, Transport, X_AGENT, X_MODE, X_OPTION, X_STOP_REASON};
use loa_core::{
    CommitError, Dom, Draft, ElementKind, MemorySink, Node, NodeId, Patch, PatchSink, SessionMode, attr,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::watch;

// ---- the agent side ----

/// Hands the client end of one in-memory pipe to the adapter.
struct PipeConnector {
    client: Mutex<Option<DuplexStream>>,
}

#[async_trait::async_trait]
impl Connector for PipeConnector {
    async fn connect(&self, _config: &SessionConfig) -> Result<Transport> {
        let stream = self.client.lock().unwrap().take().expect("one session per connector");
        let (from_agent, to_agent) = tokio::io::split(stream);
        Ok(Transport { from_agent: Box::new(from_agent), to_agent: Box::new(to_agent), child: None })
    }
}

/// The scripted agent: the test reads and writes its side of the pipe.
struct Agent {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
}

impl Agent {
    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await.unwrap();
        assert!(n > 0, "the client closed the connection");
        serde_json::from_str(&line).unwrap()
    }

    /// Reads the next message and checks it is a request for `method`.
    async fn expect_request(&mut self, method: &str) -> (Value, Value) {
        let message = self.recv().await;
        assert_eq!(message["method"], method, "unexpected message {message}");
        assert!(message.get("id").is_some(), "{method} should be a request: {message}");
        (message["id"].clone(), message["params"].clone())
    }

    async fn expect_notification(&mut self, method: &str) -> Value {
        let message = self.recv().await;
        assert_eq!(message["method"], method, "unexpected message {message}");
        assert!(message.get("id").is_none(), "{method} should be a notification: {message}");
        message["params"].clone()
    }

    /// Reads the response to the agent's request `id`.
    async fn expect_response(&mut self, id: i64) -> Value {
        let message = self.recv().await;
        assert_eq!(message["id"], id, "unexpected message {message}");
        message
    }

    async fn send(&mut self, message: Value) {
        let mut line = serde_json::to_vec(&message).unwrap();
        line.push(b'\n');
        self.writer.write_all(&line).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    async fn respond(&mut self, id: Value, result: Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result })).await;
    }

    async fn update(&mut self, update: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s-1", "update": update } }))
            .await;
    }

    async fn request(&mut self, id: i64, method: &str, mut params: Value) {
        params["sessionId"] = json!("s-1");
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await;
    }

    /// Answers `initialize` with `capabilities` and `session/new` with
    /// session `s-1` and `modes`. Returns both requests' params.
    async fn handshake(&mut self, capabilities: Value, modes: Value) -> (Value, Value) {
        let (id, init) = self.expect_request("initialize").await;
        self.respond(
            id,
            json!({ "protocolVersion": 1, "agentCapabilities": capabilities, "agentInfo": { "name": "fake-agent", "version": "0.1" } }),
        )
        .await;
        let (id, new) = self.expect_request("session/new").await;
        self.respond(id, json!({ "sessionId": "s-1", "modes": modes })).await;
        (init, new)
    }
}

fn pipe() -> (Arc<AcpAdapter>, Agent) {
    let (client, agent) = tokio::io::duplex(1 << 16);
    let connector = PipeConnector { client: Mutex::new(Some(client)) };
    let (reader, writer) = tokio::io::split(agent);
    (Arc::new(AcpAdapter::with_connector(Arc::new(connector))), Agent { reader: BufReader::new(reader), writer })
}

// ---- the session side ----

/// A memory sink that wakes waiters on every commit.
struct WatchSink {
    inner: MemorySink,
    seq: watch::Sender<u64>,
}

impl WatchSink {
    fn new() -> Arc<Self> {
        Self::from_dom(Dom::new())
    }

    fn from_dom(dom: Dom) -> Arc<Self> {
        Arc::new(Self { inner: MemorySink::from_dom(dom), seq: watch::channel(0).0 })
    }

    /// Resolves once `ready` holds for the DOM.
    async fn until(&self, ready: impl Fn(&Dom) -> bool) {
        let mut seq = self.seq.subscribe();
        seq.wait_for(|_| self.inner.with_dom(&ready)).await.unwrap();
    }

    fn dom(&self) -> Dom {
        self.inner.dom()
    }
}

impl PatchSink for WatchSink {
    fn commit_at(&self, draft: Draft, ts: i64) -> Result<Patch, CommitError> {
        let patch = self.inner.commit_at(draft, ts)?;
        self.seq.send_replace(patch.seq);
        Ok(patch)
    }
}

fn config(cwd: &Path) -> SessionConfig {
    SessionConfig {
        session_id: "app-session".to_owned(),
        node: "node-key".to_owned(),
        cwd: cwd.to_owned(),
        model: Some("fake-model".to_owned()),
        ..SessionConfig::default()
    }
}

fn nodes(dom: &Dom, kind: ElementKind) -> Vec<&Node> {
    dom.iter().filter(|n| n.kind == kind).collect()
}

fn one(dom: &Dom, kind: ElementKind) -> &Node {
    let found = nodes(dom, kind);
    assert_eq!(found.len(), 1, "expected one <{kind}>, found {}", found.len());
    found[0]
}

fn status(node: &Node) -> &str {
    node.str_attr(attr::STATUS).unwrap_or("")
}

fn has_waiting_decision(dom: &Dom) -> bool {
    dom.iter().any(|n| n.kind == ElementKind::Decision && status(n) == "waiting")
}

fn prompt(text: &str) -> Prompt {
    Prompt { text: text.to_owned(), attachments: Vec::new(), by: "node-key".to_owned() }
}

/// Starts a session against the scripted agent with plain capabilities.
async fn started(dir: &Path) -> (Arc<AcpAdapter>, Agent, Box<dyn HarnessSession>, Arc<WatchSink>) {
    let (adapter, mut agent) = pipe();
    let sink = WatchSink::new();
    let start = tokio::spawn({
        let (adapter, sink, config) = (adapter.clone(), sink.clone(), config(dir));
        async move { adapter.start(config, sink).await }
    });
    agent.handshake(json!({}), Value::Null).await;
    let session = start.await.unwrap().unwrap();
    (adapter, agent, session, sink)
}

// ---- tests ----

#[tokio::test]
async fn start_negotiates_and_hands_mcp_servers_to_session_new() {
    let dir = tempfile::tempdir().unwrap();
    let (adapter, mut agent) = pipe();
    assert!(!adapter.capabilities().has(Feature::PlanMode));
    let sink = WatchSink::new();
    let mut config = config(dir.path());
    config.mcp_servers = vec![
        McpServerSpec {
            name: "files".to_owned(),
            transport: McpTransport::Stdio {
                command: "mcp-files".to_owned(),
                args: vec!["--root".to_owned()],
                env: BTreeMap::from([("TOKEN".to_owned(), "t".to_owned())]),
            },
        },
        McpServerSpec {
            name: "web".to_owned(),
            transport: McpTransport::Http {
                url: "http://127.0.0.1:1/mcp".to_owned(),
                headers: BTreeMap::from([("Authorization".to_owned(), "Bearer x".to_owned())]),
            },
        },
    ];
    let start = tokio::spawn({
        let (adapter, sink) = (adapter.clone(), sink.clone());
        async move { adapter.start(config, sink).await }
    });
    let modes = json!({ "currentModeId": "default", "availableModes": [{ "id": "default", "name": "Default" }, { "id": "plan", "name": "Plan" }] });
    let (init, new) = agent.handshake(json!({ "mcpCapabilities": { "http": true } }), modes).await;
    let _session = start.await.unwrap().unwrap();

    assert_eq!(init["protocolVersion"], 1);
    assert_eq!(init["clientCapabilities"]["fs"], json!({ "readTextFile": true, "writeTextFile": true }));
    assert_eq!(init["clientCapabilities"]["terminal"], true);
    let cwd = dir.path().canonicalize().unwrap();
    assert_eq!(new["cwd"], cwd.to_str().unwrap());
    assert_eq!(
        new["mcpServers"],
        json!([
            { "name": "files", "command": "mcp-files", "args": ["--root"], "env": [{ "name": "TOKEN", "value": "t" }] },
            { "type": "http", "name": "web", "url": "http://127.0.0.1:1/mcp", "headers": [{ "name": "Authorization", "value": "Bearer x" }] },
        ])
    );

    let dom = sink.dom();
    let root = dom.root().unwrap();
    assert_eq!(root.str_attr(attr::HARNESS), Some("acp"));
    assert_eq!(root.str_attr(attr::ID), Some("app-session"));
    assert_eq!(root.str_attr(attr::NATIVE_ID), Some("s-1"));
    assert_eq!(root.str_attr(X_AGENT), Some("fake-agent 0.1"));
    assert_eq!(root.str_attr(X_MODE), Some("default"));
    assert_eq!(root.str_attr(attr::MODE), Some("normal"));
    // The agent offered a plan mode, so plan mode is native from now on.
    assert!(adapter.capabilities().has(Feature::PlanMode));
}

#[tokio::test]
async fn http_mcp_servers_need_agent_support() {
    let dir = tempfile::tempdir().unwrap();
    let (adapter, mut agent) = pipe();
    let mut config = config(dir.path());
    config.mcp_servers = vec![McpServerSpec {
        name: "web".to_owned(),
        transport: McpTransport::Http { url: "http://127.0.0.1:1/mcp".to_owned(), headers: BTreeMap::new() },
    }];
    let start = tokio::spawn({
        let sink = WatchSink::new();
        async move { adapter.start(config, sink).await }
    });
    let (id, _) = agent.expect_request("initialize").await;
    agent.respond(id, json!({ "protocolVersion": 1, "agentCapabilities": {} })).await;
    let result = start.await.unwrap();
    assert!(matches!(result, Err(AdapterError::Unsupported { what: "HTTP MCP servers", .. })));
}

#[tokio::test]
async fn other_protocol_versions_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (adapter, mut agent) = pipe();
    let start = tokio::spawn({
        let config = config(dir.path());
        async move { adapter.start(config, WatchSink::new()).await }
    });
    let (id, _) = agent.expect_request("initialize").await;
    agent.respond(id, json!({ "protocolVersion": 2 })).await;
    assert!(matches!(start.await.unwrap(), Err(AdapterError::Version { .. })));
}

#[tokio::test]
async fn prompt_streams_thinking_and_text_into_messages() {
    let dir = tempfile::tempdir().unwrap();
    let (_adapter, mut agent, session, sink) = started(dir.path()).await;
    let mut with_file = prompt("Explain main.rs");
    with_file.attachments.push(Attachment { path: PathBuf::from("/work/src/main file.rs"), mime: Some("text/x-rust".to_owned()) });
    session.send(with_file).await.unwrap();
    assert_eq!(session.state(), SessionState::Running);

    let (id, params) = agent.expect_request("session/prompt").await;
    assert_eq!(params["sessionId"], "s-1");
    assert_eq!(
        params["prompt"],
        json!([
            { "type": "text", "text": "Explain main.rs" },
            { "type": "resource_link", "name": "main file.rs", "uri": "file:///work/src/main%20file.rs", "mimeType": "text/x-rust" },
        ])
    );
    agent.update(json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "Let me " } })).await;
    agent.update(json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "look." } })).await;
    agent.update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "It prints " } })).await;
    agent.update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hello." } })).await;
    agent.update(json!({ "sessionUpdate": "available_commands_update", "availableCommands": [] })).await;
    agent.respond(id, json!({ "stopReason": "end_turn" })).await;
    session.wait_idle().await.unwrap();

    let dom = sink.dom();
    let turn = one(&dom, ElementKind::Turn);
    assert_eq!(status(turn), "done");
    assert!(turn.int_attr(attr::ENDED).is_some());
    let messages: Vec<&Node> = dom.children_of_kind(turn.id, ElementKind::Message).collect();
    assert_eq!(messages.len(), 3);
    assert_eq!((messages[0].str_attr(attr::ROLE), messages[0].text.as_str()), (Some("user"), "Explain main.rs"));
    assert_eq!(messages[0].str_attr(attr::BY), Some("node-key"));
    assert_eq!(messages[1].str_attr(attr::KIND), Some("thinking"));
    assert_eq!((messages[1].text.as_str(), status(messages[1])), ("Let me look.", "done"));
    assert_eq!((messages[2].text.as_str(), status(messages[2])), ("It prints hello.", "done"));
    assert_eq!(messages[2].str_attr(attr::MODEL), Some("fake-model"));
    let ext = one(&dom, ElementKind::Ext);
    assert_eq!(ext.str_attr(attr::TYPE), Some("available_commands_update"));
    assert_eq!(ext.parent, turn.id);
}

#[tokio::test]
async fn tool_calls_and_their_updates_become_calls_with_results() {
    let dir = tempfile::tempdir().unwrap();
    let (_adapter, mut agent, session, sink) = started(dir.path()).await;
    session.send(prompt("fix it")).await.unwrap();
    let (id, _) = agent.expect_request("session/prompt").await;
    agent.update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Reading." } })).await;
    agent
        .update(json!({
            "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Read src/lib.rs", "kind": "read",
            "status": "pending", "rawInput": { "path": "src/lib.rs" }
        }))
        .await;
    agent.update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "in_progress" })).await;
    agent
        .update(json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed",
            "content": [
                { "type": "content", "content": { "type": "text", "text": "fn main() {}" } },
                { "type": "diff", "path": "/x/src/lib.rs", "oldText": "a", "newText": "b" }
            ]
        }))
        .await;
    agent.update(json!({ "sessionUpdate": "tool_call", "toolCallId": "c2", "title": "Run tests", "kind": "execute", "status": "in_progress" })).await;
    agent
        .update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c2", "status": "failed", "rawOutput": { "exit": 101 } }))
        .await;
    agent.update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Done." } })).await;
    agent.respond(id, json!({ "stopReason": "end_turn" })).await;
    session.wait_idle().await.unwrap();

    let dom = sink.dom();
    let turn = one(&dom, ElementKind::Turn);
    let kinds: Vec<ElementKind> = dom.children(turn.id).map(|n| n.kind).collect();
    // Text before a call and text after it are separate messages around the calls.
    assert_eq!(
        kinds,
        [ElementKind::Message, ElementKind::Message, ElementKind::Call, ElementKind::Call, ElementKind::Message]
    );
    let calls = nodes(&dom, ElementKind::Call);
    let read = calls[0];
    assert_eq!(read.str_attr(attr::CALL_ID), Some("c1"));
    assert_eq!(read.str_attr(attr::TOOL), Some("read"));
    assert_eq!(read.str_attr(attr::TITLE), Some("Read src/lib.rs"));
    assert_eq!(status(read), "done");
    assert!(read.int_attr(attr::ENDED).is_some());
    let input = dom.children_of_kind(read.id, ElementKind::Input).next().unwrap();
    assert_eq!(serde_json::from_str::<Value>(&input.text).unwrap(), json!({ "path": "src/lib.rs" }));
    let result = dom.children_of_kind(read.id, ElementKind::Result).next().unwrap();
    assert_eq!((result.text.as_str(), result.bool_attr(attr::IS_ERROR)), ("fn main() {}", Some(false)));
    let diff = dom.children_of_kind(read.id, ElementKind::Ext).next().unwrap();
    assert_eq!(serde_json::from_str::<Value>(&diff.text).unwrap()["newText"], "b");

    let run = calls[1];
    assert_eq!(status(run), "error");
    let result = dom.children_of_kind(run.id, ElementKind::Result).next().unwrap();
    assert_eq!(result.bool_attr(attr::IS_ERROR), Some(true));
    assert_eq!(serde_json::from_str::<Value>(&result.text).unwrap(), json!({ "exit": 101 }));
}

#[tokio::test]
async fn permission_answer_returns_the_picked_option_id() {
    let dir = tempfile::tempdir().unwrap();
    let (_adapter, mut agent, session, sink) = started(dir.path()).await;
    session.send(prompt("delete build/")).await.unwrap();
    let (prompt_id, _) = agent.expect_request("session/prompt").await;
    agent
        .update(json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "rm -rf build", "kind": "execute", "status": "pending", "rawInput": { "command": "rm -rf build" } }))
        .await;
    agent
        .request(
            7,
            "session/request_permission",
            json!({
                "toolCall": { "toolCallId": "c1" },
                "options": [
                    { "optionId": "once", "name": "Allow", "kind": "allow_once" },
                    { "optionId": "always", "name": "Always allow", "kind": "allow_always" },
                    { "optionId": "no", "name": "Reject", "kind": "reject_once" }
                ]
            }),
        )
        .await;
    sink.until(has_waiting_decision).await;
    assert_eq!(session.state(), SessionState::Waiting);

    let dom = sink.dom();
    let decision = one(&dom, ElementKind::Decision);
    let call = one(&dom, ElementKind::Call);
    assert_eq!(decision.parent, call.id);
    assert_eq!(decision.int_attr(attr::CALL), Some(call.id.0 as i64));
    assert_eq!(decision.str_attr(attr::KIND), Some("approval"));
    assert_eq!(decision.str_attr(attr::REQUEST_ID), Some("7"));
    let options: Vec<&str> = decision.attr(attr::OPTIONS).unwrap().str_items().collect();
    assert_eq!(options, ["Allow", "Always allow", "Reject"]);
    assert_eq!(status(one(&dom, ElementKind::Turn)), "waiting");

    let unknown = session.answer(NodeId(9999), Answer { values: vec!["Allow".to_owned()], ..Answer::default() }).await;
    assert!(matches!(unknown, Err(AdapterError::UnknownDecision(_))));
    let answer = Answer { values: vec![loa_core::answer::ALLOW_SESSION.to_owned()], reason: None, by: "alice".to_owned() };
    session.answer(decision.id, answer).await.unwrap();
    let response = agent.expect_response(7).await;
    assert_eq!(response["result"], json!({ "outcome": { "outcome": "selected", "optionId": "always" } }));

    let dom = sink.dom();
    let decision = one(&dom, ElementKind::Decision);
    assert_eq!(status(decision), "answered");
    assert_eq!(decision.attr(attr::ANSWER).unwrap().str_items().collect::<Vec<_>>(), ["allow_session"]);
    assert_eq!(decision.str_attr(X_OPTION), Some("always"));
    assert_eq!(decision.str_attr(attr::BY), Some("alice"));
    assert_eq!(status(one(&dom, ElementKind::Turn)), "running");

    agent.update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed" })).await;
    agent.respond(prompt_id, json!({ "stopReason": "end_turn" })).await;
    session.wait_idle().await.unwrap();
    let dom = sink.dom();
    assert_eq!(status(one(&dom, ElementKind::Call)), "done");
    assert_eq!(status(one(&dom, ElementKind::Turn)), "done");
}

#[tokio::test]
async fn interrupt_cancels_the_turn_and_its_pending_permission() {
    let dir = tempfile::tempdir().unwrap();
    let (_adapter, mut agent, session, sink) = started(dir.path()).await;
    session.send(prompt("long job")).await.unwrap();
    let (prompt_id, _) = agent.expect_request("session/prompt").await;
    agent.update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Starting" } })).await;
    agent.update(json!({ "sessionUpdate": "tool_call", "toolCallId": "c1", "title": "deploy", "kind": "execute", "status": "in_progress" })).await;
    agent
        .request(
            3,
            "session/request_permission",
            json!({ "toolCall": { "toolCallId": "c1" }, "options": [{ "optionId": "ok", "name": "Allow", "kind": "allow_once" }] }),
        )
        .await;
    sink.until(has_waiting_decision).await;

    session.interrupt().await.unwrap();
    let response = agent.expect_response(3).await;
    assert_eq!(response["result"], json!({ "outcome": { "outcome": "cancelled" } }));
    let cancel = agent.expect_notification("session/cancel").await;
    assert_eq!(cancel["sessionId"], "s-1");
    // ACP lets the agent flush updates before it answers the prompt.
    agent.update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Stopping" } })).await;
    agent.respond(prompt_id, json!({ "stopReason": "cancelled" })).await;
    session.wait_idle().await.unwrap();

    let dom = sink.dom();
    let turn = one(&dom, ElementKind::Turn);
    assert_eq!(status(turn), "cancelled");
    let messages: Vec<&Node> = dom.children_of_kind(turn.id, ElementKind::Message).collect();
    // Text the tool call closed stays done; text still streaming is cancelled.
    assert_eq!(status(messages[1]), "done");
    assert_eq!((messages[2].text.as_str(), status(messages[2])), ("Stopping", "cancelled"));
    assert_eq!(status(one(&dom, ElementKind::Call)), "cancelled");
    assert_eq!(status(one(&dom, ElementKind::Decision)), "cancelled");
    assert_eq!(session.state(), SessionState::Idle);
    // Idle again: the next prompt goes through.
    session.send(prompt("again")).await.unwrap();
    agent.expect_request("session/prompt").await;
}

#[tokio::test]
async fn a_second_prompt_mid_turn_is_busy() {
    let dir = tempfile::tempdir().unwrap();
    let (_adapter, mut agent, session, sink) = started(dir.path()).await;
    session.send(prompt("one")).await.unwrap();
    assert!(matches!(session.send(prompt("two")).await, Err(AdapterError::Busy)));
    let (id, _) = agent.expect_request("session/prompt").await;
    agent.respond(id, json!({ "stopReason": "max_tokens" })).await;
    session.wait_idle().await.unwrap();
    let dom = sink.dom();
    let turn = one(&dom, ElementKind::Turn);
    assert_eq!(nodes(&dom, ElementKind::Turn).len(), 1);
    assert_eq!(status(turn), "error");
    assert_eq!(turn.str_attr(X_STOP_REASON), Some("max_tokens"));
}

#[tokio::test]
async fn plans_become_todos_and_mode_changes_set_the_session_mode() {
    let dir = tempfile::tempdir().unwrap();
    let (_adapter, mut agent, session, sink) = started(dir.path()).await;
    session.send(prompt("plan it")).await.unwrap();
    let (id, _) = agent.expect_request("session/prompt").await;
    agent
        .update(json!({ "sessionUpdate": "plan", "entries": [
            { "content": "Read code", "priority": "high", "status": "in_progress" },
            { "content": "Write fix", "priority": "medium", "status": "pending" },
            { "content": "Run tests", "priority": "low", "status": "pending" }
        ] }))
        .await;
    agent
        .update(json!({ "sessionUpdate": "plan", "entries": [
            { "content": "Read code", "priority": "high", "status": "completed" },
            { "content": "Write the fix", "priority": "medium", "status": "in_progress" }
        ] }))
        .await;
    agent.update(json!({ "sessionUpdate": "current_mode_update", "currentModeId": "plan" })).await;
    agent.respond(id, json!({ "stopReason": "end_turn" })).await;
    session.wait_idle().await.unwrap();

    let dom = sink.dom();
    let root = dom.root().unwrap();
    let todos: Vec<(&str, &str)> = dom
        .children_of_kind(root.id, ElementKind::Todo)
        .map(|t| (t.text.as_str(), t.str_attr(attr::STATE).unwrap()))
        .collect();
    assert_eq!(todos, [("Read code", "done"), ("Write the fix", "in_progress")]);
    assert_eq!(root.str_attr(attr::MODE), Some("plan"));
    assert_eq!(root.str_attr(X_MODE), Some("plan"));
}

#[tokio::test]
async fn set_mode_switches_to_the_agents_plan_mode() {
    let dir = tempfile::tempdir().unwrap();
    let (adapter, mut agent) = pipe();
    let sink = WatchSink::new();
    let start = tokio::spawn({
        let (adapter, sink, config) = (adapter.clone(), sink.clone(), config(dir.path()));
        async move { adapter.start(config, sink).await }
    });
    let modes = json!({ "currentModeId": "ask", "availableModes": [{ "id": "ask", "name": "Ask" }, { "id": "plan", "name": "Plan" }] });
    agent.handshake(json!({}), modes).await;
    let session = Arc::new(start.await.unwrap().unwrap());

    let switch = tokio::spawn({
        let session = session.clone();
        async move { session.set_mode(SessionMode::Plan).await }
    });
    let (id, params) = agent.expect_request("session/set_mode").await;
    assert_eq!(params, json!({ "sessionId": "s-1", "modeId": "plan" }));
    agent.respond(id, json!({})).await;
    switch.await.unwrap().unwrap();
    assert_eq!(sink.dom().root().unwrap().str_attr(attr::MODE), Some("plan"));

    // Back to normal is the mode the session started in, since there is no "default".
    let switch = tokio::spawn({
        let session = session.clone();
        async move { session.set_mode(SessionMode::Normal).await }
    });
    let (id, params) = agent.expect_request("session/set_mode").await;
    assert_eq!(params["modeId"], "ask");
    agent.respond(id, json!({})).await;
    switch.await.unwrap().unwrap();
    assert_eq!(sink.dom().root().unwrap().str_attr(attr::MODE), Some("normal"));
}

#[tokio::test]
async fn file_requests_stay_inside_the_session_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    std::fs::write(cwd.join("notes.txt"), "a\nb\nc\n").unwrap();
    let (_adapter, mut agent, _session, _sink) = started(&cwd).await;

    let new_file = cwd.join("src/new.rs");
    agent.request(1, "fs/write_text_file", json!({ "path": new_file, "content": "fn x() {}" })).await;
    let response = agent.expect_response(1).await;
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(std::fs::read_to_string(&new_file).unwrap(), "fn x() {}");

    agent.request(2, "fs/read_text_file", json!({ "path": cwd.join("notes.txt"), "line": 2, "limit": 1 })).await;
    assert_eq!(agent.expect_response(2).await["result"]["content"], "b\n");

    let outside = dir.path().join("outside.txt");
    agent.request(3, "fs/write_text_file", json!({ "path": outside, "content": "x" })).await;
    assert!(agent.expect_response(3).await.get("error").is_some());
    assert!(!outside.exists());

    let climbing = cwd.join("../climb.txt");
    agent.request(4, "fs/write_text_file", json!({ "path": climbing, "content": "x" })).await;
    assert!(agent.expect_response(4).await.get("error").is_some());
    assert!(!dir.path().join("climb.txt").exists());

    std::fs::write(dir.path().join("secret.txt"), "s").unwrap();
    agent.request(5, "fs/read_text_file", json!({ "path": dir.path().join("secret.txt") })).await;
    assert!(agent.expect_response(5).await.get("error").is_some());

    agent.request(6, "fs/read_text_file", json!({ "path": cwd.join("missing.txt") })).await;
    assert_eq!(agent.expect_response(6).await["error"]["code"], -32002);
}

#[tokio::test]
async fn terminals_run_inside_the_session_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();
    let (_adapter, mut agent, _session, _sink) = started(&cwd).await;

    agent
        .request(1, "terminal/create", json!({ "command": "sh", "args": ["-c", "pwd; echo \"$GREETING\" >&2; exit 3"], "env": [{ "name": "GREETING", "value": "hi" }] }))
        .await;
    let id = agent.expect_response(1).await["result"]["terminalId"].clone();
    agent.request(2, "terminal/wait_for_exit", json!({ "terminalId": id })).await;
    assert_eq!(agent.expect_response(2).await["result"]["exitCode"], 3);
    agent.request(3, "terminal/output", json!({ "terminalId": id })).await;
    let output = agent.expect_response(3).await["result"].clone();
    let text = output["output"].as_str().unwrap();
    assert!(text.contains(cwd.to_str().unwrap()), "{text}");
    assert!(text.contains("hi"), "{text}");
    assert_eq!(output["truncated"], false);
    assert_eq!(output["exitStatus"]["exitCode"], 3);

    agent.request(4, "terminal/create", json!({ "command": "sh", "args": ["-c", "printf 0123456789"], "outputByteLimit": 4 })).await;
    let id = agent.expect_response(4).await["result"]["terminalId"].clone();
    agent.request(5, "terminal/wait_for_exit", json!({ "terminalId": id })).await;
    agent.expect_response(5).await;
    agent.request(6, "terminal/output", json!({ "terminalId": id })).await;
    let output = agent.expect_response(6).await["result"].clone();
    assert_eq!((output["output"].as_str(), output["truncated"].as_bool()), (Some("6789"), Some(true)));

    agent.request(7, "terminal/create", json!({ "command": "sleep", "args": ["600"] })).await;
    let id = agent.expect_response(7).await["result"]["terminalId"].clone();
    agent.request(8, "terminal/kill", json!({ "terminalId": id })).await;
    agent.expect_response(8).await;
    agent.request(9, "terminal/wait_for_exit", json!({ "terminalId": id })).await;
    assert_eq!(agent.expect_response(9).await["result"]["signal"], "SIGKILL");
    agent.request(10, "terminal/release", json!({ "terminalId": id })).await;
    agent.expect_response(10).await;
    agent.request(11, "terminal/output", json!({ "terminalId": id })).await;
    assert!(agent.expect_response(11).await.get("error").is_some());

    agent.request(12, "terminal/create", json!({ "command": "true", "cwd": dir.path() })).await;
    assert!(agent.expect_response(12).await.get("error").is_some());
}

#[tokio::test]
async fn resume_loads_the_session_without_duplicating_history() {
    let dir = tempfile::tempdir().unwrap();
    // First run: one turn and a plan.
    let (_adapter, mut agent, session, sink) = started(dir.path()).await;
    session.send(prompt("hello")).await.unwrap();
    let (id, _) = agent.expect_request("session/prompt").await;
    agent.update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hi" } })).await;
    agent.update(json!({ "sessionUpdate": "plan", "entries": [{ "content": "step", "priority": "low", "status": "pending" }] })).await;
    agent.respond(id, json!({ "stopReason": "end_turn" })).await;
    session.wait_idle().await.unwrap();
    session.shutdown().await.unwrap();
    assert_eq!(session.state(), SessionState::Closed);
    let before = sink.dom();
    let seq_before_load = before.seq();

    // Second run: the agent replays the history while loading.
    let (adapter, mut agent) = pipe();
    let sink = WatchSink::from_dom(before.clone());
    let resume = tokio::spawn({
        let (adapter, sink, config) = (adapter.clone(), sink.clone(), config(dir.path()));
        async move { adapter.resume(config, &before, sink).await }
    });
    let (id, _) = agent.expect_request("initialize").await;
    agent.respond(id, json!({ "protocolVersion": 1, "agentCapabilities": { "loadSession": true } })).await;
    let (id, params) = agent.expect_request("session/load").await;
    assert_eq!(params["sessionId"], "s-1");
    assert_eq!(params["mcpServers"], json!([]));
    agent.update(json!({ "sessionUpdate": "user_message_chunk", "content": { "type": "text", "text": "hello" } })).await;
    agent.update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hi" } })).await;
    agent.respond(id, Value::Null).await;
    let session = resume.await.unwrap().unwrap();
    assert_eq!(sink.dom().seq(), seq_before_load, "loading wrote nothing");

    session.send(prompt("next")).await.unwrap();
    let (id, _) = agent.expect_request("session/prompt").await;
    agent.update(json!({ "sessionUpdate": "plan", "entries": [{ "content": "step", "priority": "low", "status": "completed" }] })).await;
    agent.respond(id, json!({ "stopReason": "end_turn" })).await;
    session.wait_idle().await.unwrap();

    let dom = sink.dom();
    let turns = nodes(&dom, ElementKind::Turn);
    assert_eq!(turns.len(), 2);
    assert_eq!(dom.children(turns[0].id).count(), 2, "the replay added nothing to the first turn");
    let todos = nodes(&dom, ElementKind::Todo);
    assert_eq!(todos.len(), 1, "the plan update edited the existing todo");
    assert_eq!(todos[0].str_attr(attr::STATE), Some("done"));
}

#[tokio::test]
async fn resume_needs_load_session() {
    let dir = tempfile::tempdir().unwrap();
    let sink = MemorySink::new();
    sink.commit(Draft::insert(
        NodeId::DOCUMENT,
        ElementKind::Session,
        [
            (attr::ID, "a".into()),
            (attr::HARNESS, "acp".into()),
            (attr::NODE, "n".into()),
            (attr::CREATED, 1i64.into()),
            (attr::NATIVE_ID, "s-1".into()),
        ],
        "",
    ))
    .unwrap();
    let dom = sink.dom();
    let (adapter, mut agent) = pipe();
    let resume = tokio::spawn({
        let config = config(dir.path());
        async move { adapter.resume(config, &dom, WatchSink::from_dom(dom.clone())).await }
    });
    let (id, _) = agent.expect_request("initialize").await;
    agent.respond(id, json!({ "protocolVersion": 1, "agentCapabilities": {} })).await;
    assert!(matches!(resume.await.unwrap(), Err(AdapterError::Unsupported { what: "resume", .. })));
}

#[tokio::test]
async fn agent_exit_mid_turn_fails_the_turn_and_closes_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let (_adapter, mut agent, session, sink) = started(dir.path()).await;
    session.send(prompt("work")).await.unwrap();
    agent.expect_request("session/prompt").await;
    agent.update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "partial" } })).await;
    drop(agent);
    session.wait_idle().await.unwrap();
    assert_eq!(session.state(), SessionState::Closed);
    let dom = sink.dom();
    let turn = one(&dom, ElementKind::Turn);
    assert_eq!(status(turn), "error");
    let assistant = dom.children_of_kind(turn.id, ElementKind::Message).nth(1).unwrap();
    assert_eq!(status(assistant), "error");
    assert!(matches!(session.send(prompt("more")).await, Err(AdapterError::Closed)));
}

#[cfg(unix)]
#[tokio::test]
async fn command_connector_runs_the_agent_process() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("agent.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         read line\n\
         printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":1,\"agentInfo\":{\"name\":\"%s\"}}}\\n' \"$AGENT_NAME\"\n\
         read line\n\
         printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"sessionId\":\"from-script\"}}'\n\
         cat > /dev/null\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let adapter = AcpAdapter::new(&script, Vec::<String>::new());
    let sink = WatchSink::new();
    let mut config = config(dir.path());
    config.env.insert("AGENT_NAME".to_owned(), "script-agent".to_owned());
    let session = adapter.start(config, sink.clone()).await.unwrap();
    let dom = sink.dom();
    let root = dom.root().unwrap();
    assert_eq!(root.str_attr(attr::NATIVE_ID), Some("from-script"));
    assert_eq!(root.str_attr(X_AGENT), Some("script-agent"));
    session.shutdown().await.unwrap();
    assert_eq!(session.state(), SessionState::Closed);

    let missing = AcpAdapter::new(dir.path().join("no-such-agent"), Vec::<String>::new());
    let result = missing.start(self::config(dir.path()), WatchSink::new()).await;
    assert!(matches!(result, Err(AdapterError::NotInstalled(_))));
}
