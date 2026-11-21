//! A running ACP session and its ordered inbound event handler.

use super::{HARNESS, host::Host, mapper::{AcpMapper, Input, Reply}};
use crate::kit::{EventMapper, Framing, Inbound, JsonRpc2, RpcError, StdioRpc};
use crate::kit::rpc::Writer;
use async_trait::async_trait;
use loa_contracts::harness::{AdapterError, Answer, HarnessModel, HarnessSession, ModelSelection, Prompt, Result, SessionState};
use loa_core::{DomWriter, NodeId, PatchSink, SessionMode};
use loa_util::LockExt;
use serde_json::{Value, json};
use std::path::Path;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, OnceLock, atomic::{AtomicBool, Ordering}};
use tokio::sync::watch;

pub(crate) struct Shared {
    pub mapper: Mutex<AcpMapper>,
    pub sink: Arc<dyn PatchSink>,
    pub host: Arc<Host>,
    pub session_id: OnceLock<String>,
    pub writer: OnceLock<Writer<JsonRpc2>>,
    pub writer_ready: tokio::sync::Notify,
    pub state: watch::Sender<SessionState>,
    pub shutting_down: AtomicBool,
    pub modes: Mutex<Modes>,
    pub models: Mutex<Option<Vec<HarnessModel>>>,
}

#[derive(Default)]
pub(crate) struct Modes { pub available: Vec<String>, pub initial: Option<String> }

impl Shared {
    pub fn session_id(&self) -> &str { self.session_id.get().map_or("", String::as_str) }

    pub fn feed(&self, input: Input) -> Result<()> {
        let replies = {
            let mut mapper = self.mapper.locked();
            let result = mapper.apply(input, &DomWriter::new(&*self.sink));
            self.state.send_replace(mapper.ledger().state());
            result?
        };
        for reply in replies { self.reply(reply)?; }
        Ok(())
    }

    fn reply(&self, reply: Reply) -> Result<()> {
        let framing = JsonRpc2 { header: true };
        let frame = match reply {
            Reply::Respond { id, result } => framing.response(id, "", Ok(result)),
            Reply::Reject { id, message } => framing.response(id, "", Err(RpcError::Command { command: "request".into(), message, code: Some("-32602".into()) })),
            Reply::Cancel => framing.notification("session/cancel", json!({"sessionId": self.session_id()})),
        };
        self.writer.get().ok_or(AdapterError::Closed)?.send(&frame)?;
        Ok(())
    }

    fn report(&self, result: Result<()>) {
        if let Err(error) = result { tracing::error!(%error, "ACP event handling failed"); }
    }
}

#[async_trait]
impl Inbound for Shared {
    async fn on_request(&self, method: &str, params: Value) -> std::result::Result<Value, RpcError> {
        self.host.serve(method, params).await
    }
    fn defers_requests(&self) -> bool { true }
    async fn on_deferred(&self, id: Value, method: &str, params: Value) {
        if self.writer.get().is_none() { self.writer_ready.notified().await; }
        if method == "session/request_permission" {
            self.report(self.feed(Input::Permission { id, params }));
        } else {
            let host = self.host.clone();
            let writer = self.writer.get().cloned();
            let method = method.to_owned();
            tokio::spawn(async move {
                let result = host.serve(&method, params).await;
                if let Some(writer) = writer {
                    let frame = JsonRpc2 { header: true }.response(id, &method, result);
                    if let Err(error) = writer.send(&frame) { tracing::warn!(%error, "couldn't answer ACP request"); }
                }
            });
        }
    }
    async fn on_notification(&self, method: &str, params: Value) {
        if method == "session/update" { self.report(self.feed(Input::Update(params))); }
    }
    async fn on_response(&self, id: &str, result: &std::result::Result<Value, RpcError>) {
        let result = match result { Ok(value) => Ok(value.clone()), Err(error) => Err(error.to_string()) };
        self.report(self.feed(Input::Response { id: id.to_owned(), result }));
    }
    async fn on_closed(&self, _: Option<ExitStatus>, _: &str) {
        self.report(self.feed(Input::Closed { shutdown: self.shutting_down.load(Ordering::Relaxed) }));
        self.host.release_all();
    }
}

pub struct AcpSession { pub(crate) shared: Arc<Shared>, pub(crate) rpc: Arc<StdioRpc<JsonRpc2>> }

impl AcpSession {
    pub fn native_id(&self) -> &str { self.shared.session_id() }
    pub(crate) async fn switch_mode(shared: &Shared, rpc: &StdioRpc<JsonRpc2>, mode: &str) -> Result<()> {
        rpc.request_as(json!(rpc.next_id()), "session/set_mode", json!({"sessionId": shared.session_id(), "modeId": mode})).await?;
        shared.mapper.locked().mode_changed(&DomWriter::new(&*shared.sink), mode)?;
        Ok(())
    }
    pub(crate) async fn switch_model(shared: &Shared, rpc: &StdioRpc<JsonRpc2>, model: &str) -> Result<()> {
        rpc.request_as(json!(rpc.next_id()), "session/set_model", json!({"sessionId": shared.session_id(), "modelId": model})).await?;
        shared.mapper.locked().model_changed(&DomWriter::new(&*shared.sink), model)?;
        Ok(())
    }
    pub(crate) fn mode_id(shared: &Shared, mode: SessionMode) -> Option<String> {
        let modes = shared.modes.locked();
        let has = |id: &str| modes.available.iter().any(|m| m == id);
        match mode {
            SessionMode::Plan => has("plan").then(|| "plan".to_owned()),
            SessionMode::Normal => if has("default") { Some("default".into()) } else { modes.initial.as_ref().filter(|m| m.as_str() != "plan").cloned().or_else(|| modes.available.iter().find(|m| m.as_str() != "plan").cloned()) },
        }
    }
}

fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for byte in path.as_os_str().as_encoded_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => uri.push(char::from(*byte)),
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri
}
fn prompt_blocks(prompt: &Prompt) -> Vec<Value> {
    let mut blocks = Vec::with_capacity(1 + prompt.attachments.len());
    blocks.push(json!({"type": "text", "text": prompt.text}));
    for attachment in &prompt.attachments {
        let name = attachment.path.file_name().map_or_else(|| attachment.path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
        let mut block = json!({"type": "resource_link", "name": name, "uri": file_uri(&attachment.path)});
        if let Some(mime) = &attachment.mime { block["mimeType"] = json!(mime); }
        blocks.push(block);
    }
    blocks
}

#[async_trait]
impl HarnessSession for AcpSession {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        let id = self.rpc.next_id();
        let params = json!({"sessionId": self.shared.session_id(), "prompt": prompt_blocks(&prompt)});
        let frame = JsonRpc2 { header: true }.request(&json!(id), "session/prompt", params);
        let sent = {
            let mut mapper = self.shared.mapper.locked();
            match mapper.ledger().state() { SessionState::Closed => return Err(AdapterError::Closed), SessionState::Idle => {}, _ => return Err(AdapterError::Busy) }
            mapper.apply(Input::Prompt { id: id.to_string(), prompt }, &DomWriter::new(&*self.shared.sink))?;
            self.shared.state.send_replace(mapper.ledger().state());
            self.rpc.send(&frame)
        };
        if let Err(error) = sent {
            self.shared.feed(Input::Response { id: id.to_string(), result: Err(error.to_string()) })?;
            return Err(error.into());
        }
        Ok(())
    }
    async fn interrupt(&self) -> Result<()> { self.shared.feed(Input::Interrupt) }
    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        let reply = {
            let mut mapper = self.shared.mapper.locked();
            let reply = mapper.answer(decision, &answer, &DomWriter::new(&*self.shared.sink));
            self.shared.state.send_replace(mapper.ledger().state());
            reply?
        };
        if let Some(reply) = reply { self.shared.reply(reply)?; }
        Ok(())
    }
    async fn set_mode(&self, mode: SessionMode) -> Result<()> {
        let id = Self::mode_id(&self.shared, mode).ok_or(AdapterError::Unsupported { harness: HARNESS, what: "set_mode" })?;
        if self.shared.mapper.locked().mode_id() == Some(id.as_str()) { return Ok(()); }
        Self::switch_mode(&self.shared, &self.rpc, &id).await
    }
    async fn models(&self) -> Result<Vec<HarnessModel>> {
        self.shared.models.locked().clone().ok_or(AdapterError::Unsupported { harness: HARNESS, what: "listing models" })
    }
    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        let offered = self.shared.models.locked().as_ref().map(|models| models.iter().any(|m| m.id == selection.model));
        match (offered, &selection.role) {
            (None, _) => Err(AdapterError::Unsupported { harness: HARNESS, what: "set_model" }),
            (_, Some(_)) => Err(AdapterError::Unsupported { harness: HARNESS, what: "model roles" }),
            (Some(false), None) => Err(AdapterError::Protocol(format!("the agent doesn't offer model {}", selection.model))),
            (Some(true), None) => Self::switch_model(&self.shared, &self.rpc, &selection.model).await,
        }
    }
    fn harness(&self) -> &'static str { HARNESS }
    fn state(&self) -> SessionState { *self.shared.state.borrow() }
    async fn wait_idle(&self) -> Result<()> {
        self.shared.state.subscribe().wait_for(|s| matches!(s, SessionState::Idle | SessionState::Closed)).await.map_err(|_| AdapterError::Closed)?;
        Ok(())
    }
    async fn shutdown(&self) -> Result<()> {
        self.shared.shutting_down.store(true, Ordering::Relaxed);
        self.rpc.kill();
        self.rpc.closed().await;
        self.shared.host.release_all();
        if self.state() != SessionState::Closed { self.shared.feed(Input::Closed { shutdown: true })?; }
        Ok(())
    }
}

impl Drop for AcpSession {
    fn drop(&mut self) {
        self.shared.shutting_down.store(true, Ordering::Relaxed);
        self.rpc.kill();
    }
}
