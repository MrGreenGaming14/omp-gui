//! A running ACP session: the [`HarnessSession`] side and the handler for
//! everything the agent sends.

use loa_util::LockExt;
use crate::fs;
use crate::project::Projector;
use crate::rpc::{Inbound, Rpc, RpcError, RpcResult};
use crate::terminal::Terminals;
use crate::wire::{
    CreateTerminalRequest, PermissionOption, PermissionOptionKind, ReadTextFileRequest, RequestPermissionRequest,
    TerminalRef, WriteTextFileRequest, code,
};
use async_trait::async_trait;
use loa_adapter::{AdapterError, Answer, HarnessModel, HarnessSession, ModelSelection, Prompt, Result, SessionState};
use loa_core::{NodeId, SessionMode, answer};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tokio::process::Child;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// State shared by the session handle and the connection's reader task.
pub(crate) struct Shared {
    pub rpc: Arc<Rpc>,
    pub projector: Mutex<Projector>,
    /// The agent's session id, known once `session/new` or `session/load` answered.
    pub session_id: OnceLock<String>,
    /// Canonical session cwd.
    pub root: PathBuf,
    pub terminals: Terminals,
    pub state: watch::Sender<SessionState>,
    pub shutting_down: AtomicBool,
    /// Mode ids the agent offers, and the one it started in.
    pub modes: Mutex<Modes>,
    /// The models the agent offers (unstable `session/set_model`); `None`
    /// when it offers none.
    pub models: Mutex<Option<Vec<HarnessModel>>>,
}

#[derive(Default)]
pub(crate) struct Modes {
    pub available: Vec<String>,
    pub initial: Option<String>,
}

impl Shared {
    pub fn projector(&self) -> MutexGuard<'_, Projector> {
        // Projector methods commit patches one by one and keep no invariant
        // across a panic that a later call could trip over.
        self.projector.locked()
    }

    pub fn session_id(&self) -> &str {
        self.session_id.get().map_or("", String::as_str)
    }

    /// Publishes the session state after the projector changed.
    fn publish(&self, projector: &Projector) {
        self.state.send_replace(projector.state());
    }

    /// Answers permission requests the session no longer waits on.
    fn cancel_permissions(&self, ids: Vec<Value>) {
        for id in ids {
            self.rpc.respond(id, Ok(json!({ "outcome": { "outcome": "cancelled" } })));
        }
    }

    fn spawn_reply<F>(self: &Arc<Self>, id: Value, work: F)
    where
        F: Future<Output = RpcResult> + Send + 'static,
    {
        let shared = self.clone();
        tokio::spawn(async move {
            let result = work.await;
            shared.rpc.respond(id, result);
        });
    }
}

fn params<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::remote(code::INVALID_PARAMS, e.to_string()))
}

impl Inbound for Shared {
    fn on_request(self: &Arc<Self>, id: Value, method: &str, params_json: Value) {
        match method {
            "session/request_permission" => match params::<RequestPermissionRequest>(params_json) {
                Ok(request) => {
                    let mut projector = self.projector();
                    projector.permission(id, request);
                    self.publish(&projector);
                }
                Err(e) => self.rpc.respond(id, Err(e)),
            },
            "fs/read_text_file" => {
                let shared = self.clone();
                self.spawn_reply(id, async move {
                    let request: ReadTextFileRequest = params(params_json)?;
                    let content = fs::read_text_file(&shared.root, request).await?;
                    Ok(json!({ "content": content }))
                });
            }
            "fs/write_text_file" => {
                let shared = self.clone();
                self.spawn_reply(id, async move {
                    let request: WriteTextFileRequest = params(params_json)?;
                    fs::write_text_file(&shared.root, request).await?;
                    Ok(Value::Null)
                });
            }
            "terminal/create" => {
                let result = params::<CreateTerminalRequest>(params_json).and_then(|r| self.terminals.create(r));
                self.rpc.respond(id, result);
            }
            "terminal/output" => {
                let result = params::<TerminalRef>(params_json).and_then(|r| self.terminals.output(&r.terminal_id));
                self.rpc.respond(id, result);
            }
            "terminal/kill" => {
                let result = params::<TerminalRef>(params_json).and_then(|r| self.terminals.kill(&r.terminal_id));
                self.rpc.respond(id, result);
            }
            "terminal/release" => {
                let result = params::<TerminalRef>(params_json).and_then(|r| self.terminals.release(&r.terminal_id));
                self.rpc.respond(id, result);
            }
            "terminal/wait_for_exit" => {
                let shared = self.clone();
                self.spawn_reply(id, async move {
                    let request: TerminalRef = params(params_json)?;
                    shared.terminals.wait_for_exit(&request.terminal_id).await
                });
            }
            other => {
                self.rpc.respond(id, Err(RpcError::remote(code::METHOD_NOT_FOUND, format!("method not found: {other}"))));
            }
        }
    }

    fn on_notification(&self, method: &str, params: Value) {
        if method != "session/update" {
            return;
        }
        let Value::Object(mut params) = params else { return };
        let ours = match (self.session_id.get(), params.get("sessionId").and_then(Value::as_str)) {
            (Some(ours), Some(theirs)) => ours == theirs,
            _ => true,
        };
        if !ours {
            return;
        }
        let Some(update) = params.remove("update") else { return };
        let mut projector = self.projector();
        projector.update(update);
        self.publish(&projector);
    }

    fn on_response(&self, id: u64, result: &RpcResult) {
        let mut projector = self.projector();
        let waiting = projector.on_response(id, result);
        self.publish(&projector);
        drop(projector);
        self.cancel_permissions(waiting);
    }

    fn on_closed(&self) {
        let shutdown = self.shutting_down.load(Ordering::Relaxed);
        let mut projector = self.projector();
        projector.close(shutdown);
        self.publish(&projector);
        drop(projector);
        self.terminals.release_all();
    }
}

/// One running ACP agent session.
pub struct AcpSession {
    shared: Arc<Shared>,
    child: Mutex<Option<Child>>,
    reader: Mutex<Option<JoinHandle<()>>>,
}

impl AcpSession {
    pub(crate) fn new(shared: Arc<Shared>, child: Option<Child>, reader: JoinHandle<()>) -> Self {
        Self { shared, child: Mutex::new(child), reader: Mutex::new(Some(reader)) }
    }

    /// The agent's own session id (`<session native_id>`).
    pub fn native_id(&self) -> &str {
        self.shared.session_id()
    }

    /// Switches the agent to mode `mode_id` and records it on the session.
    pub(crate) async fn switch_mode(shared: &Shared, mode_id: &str) -> Result<()> {
        shared.rpc.request("session/set_mode", json!({ "sessionId": shared.session_id(), "modeId": mode_id })).await?;
        let mut projector = shared.projector();
        projector.mode_changed(mode_id);
        Ok(())
    }

    /// Switches the agent to model `model_id` and records it on the session.
    pub(crate) async fn switch_model(shared: &Shared, model_id: &str) -> Result<()> {
        shared.rpc.request("session/set_model", json!({ "sessionId": shared.session_id(), "modelId": model_id })).await?;
        shared.projector().model_changed(model_id);
        Ok(())
    }

    /// Picks the agent mode that stands for `mode`: `plan` for plan mode;
    /// `default`, else the starting mode, else the first non-plan mode otherwise.
    pub(crate) fn mode_id(shared: &Shared, mode: SessionMode) -> Option<String> {
        let modes = shared.modes.locked();
        let has = |id: &str| modes.available.iter().any(|m| m == id);
        match mode {
            SessionMode::Plan => has("plan").then(|| "plan".to_owned()),
            SessionMode::Normal => {
                if has("default") {
                    Some("default".to_owned())
                } else if let Some(initial) = modes.initial.as_ref().filter(|m| m.as_str() != "plan") {
                    Some(initial.clone())
                } else {
                    modes.available.iter().find(|m| m.as_str() != "plan").cloned()
                }
            }
        }
    }
}

/// Finds the offered option `value` names: its id, its label, or a
/// [`loa_core::answer`] constant matched by option kind.
fn pick_option<'a>(options: &'a [PermissionOption], value: &str) -> Option<&'a PermissionOption> {
    if let Some(option) = options.iter().find(|o| o.option_id == value).or_else(|| options.iter().find(|o| o.name == value)) {
        return Some(option);
    }
    let kinds: &[PermissionOptionKind] = match value {
        answer::ALLOW_ONCE => &[PermissionOptionKind::AllowOnce],
        answer::ALLOW_SESSION => &[PermissionOptionKind::AllowAlways],
        answer::DENY => &[PermissionOptionKind::RejectOnce, PermissionOptionKind::RejectAlways],
        _ => &[],
    };
    kinds.iter().find_map(|kind| options.iter().find(|o| o.kind == *kind))
}

/// `file://` URI of an absolute path, percent-encoding everything outside
/// the unreserved set and `/`.
fn file_uri(path: &std::path::Path) -> String {
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
    blocks.push(json!({ "type": "text", "text": prompt.text }));
    for attachment in &prompt.attachments {
        let name = attachment.path.file_name().map_or_else(
            || attachment.path.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        let mut block = json!({ "type": "resource_link", "name": name, "uri": file_uri(&attachment.path) });
        if let Some(mime) = &attachment.mime {
            block["mimeType"] = json!(mime);
        }
        blocks.push(block);
    }
    blocks
}

#[async_trait]
impl HarnessSession for AcpSession {
    async fn send(&self, prompt: Prompt) -> Result<()> {
        let shared = &self.shared;
        let id = shared.rpc.next_id();
        {
            let mut projector = shared.projector();
            if projector.is_closed() {
                return Err(AdapterError::Closed);
            }
            if projector.has_turn() {
                return Err(AdapterError::Busy);
            }
            projector.begin_turn(&prompt)?;
            projector.set_prompt_request(id);
            shared.publish(&projector);
        }
        let params = json!({ "sessionId": shared.session_id(), "prompt": prompt_blocks(&prompt) });
        // The reader ends the turn when the response arrives; nobody awaits it here.
        if let Err(e) = shared.rpc.start_request(id, "session/prompt", params) {
            let mut projector = shared.projector();
            let waiting = projector.end_turn(crate::project::TurnEnd::Failed(e.to_string()));
            shared.publish(&projector);
            drop(projector);
            shared.cancel_permissions(waiting);
            return Err(e.into());
        }
        Ok(())
    }

    async fn interrupt(&self) -> Result<()> {
        let shared = &self.shared;
        let waiting = {
            let mut projector = shared.projector();
            if !projector.has_turn() {
                return Ok(());
            }
            projector.mark_interrupted();
            let waiting = projector.cancel_decisions();
            shared.publish(&projector);
            waiting
        };
        // ACP: the client answers pending permission requests as cancelled
        // when it cancels the turn.
        shared.cancel_permissions(waiting);
        shared.rpc.notify("session/cancel", json!({ "sessionId": shared.session_id() }))?;
        Ok(())
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> Result<()> {
        let shared = &self.shared;
        let mut projector = shared.projector();
        let pending = projector.decision(decision).ok_or(AdapterError::UnknownDecision(decision))?;
        let value = answer.values.first().ok_or_else(|| AdapterError::Protocol("the answer is empty".to_owned()))?;
        let option = pick_option(&pending.options, value)
            .ok_or_else(|| AdapterError::Protocol(format!("`{value}` matches none of the offered options")))?
            .clone();
        let rpc_id = projector
            .answer(decision, &option, &answer.by, answer.reason.as_deref())
            .ok_or(AdapterError::UnknownDecision(decision))?;
        shared.publish(&projector);
        drop(projector);
        shared.rpc.respond(rpc_id, Ok(json!({ "outcome": { "outcome": "selected", "optionId": option.option_id } })));
        Ok(())
    }

    async fn set_mode(&self, mode: SessionMode) -> Result<()> {
        let mode_id = Self::mode_id(&self.shared, mode)
            .ok_or(AdapterError::Unsupported { harness: crate::project::HARNESS, what: "set_mode" })?;
        let current = self.shared.projector().mode_id().map(str::to_owned);
        if current.as_deref() == Some(mode_id.as_str()) {
            return Ok(());
        }
        Self::switch_mode(&self.shared, &mode_id).await
    }

    /// The models the agent offered in `session/new` or `session/load`.
    async fn models(&self) -> Result<Vec<HarnessModel>> {
        self.shared.models.locked().clone().ok_or(AdapterError::Unsupported { harness: crate::project::HARNESS, what: "listing models" })
    }

    /// `session/set_model`, for agents that offered models.
    async fn set_model(&self, selection: ModelSelection) -> Result<()> {
        let offered = self.shared.models.locked().as_ref().map(|models| models.iter().any(|m| m.id == selection.model));
        match (offered, &selection.role) {
            (None, _) => Err(AdapterError::Unsupported { harness: crate::project::HARNESS, what: "set_model" }),
            (_, Some(_)) => Err(AdapterError::Unsupported { harness: crate::project::HARNESS, what: "model roles" }),
            (Some(false), None) => Err(AdapterError::Protocol(format!("the agent doesn't offer model {}", selection.model))),
            (Some(true), None) => Self::switch_model(&self.shared, &selection.model).await,
        }
    }

    fn harness(&self) -> &'static str {
        crate::project::HARNESS
    }

    fn state(&self) -> SessionState {
        *self.shared.state.borrow()
    }

    async fn wait_idle(&self) -> Result<()> {
        let mut state = self.shared.state.subscribe();
        state
            .wait_for(|s| matches!(s, SessionState::Idle | SessionState::Closed))
            .await
            .map_err(|_| AdapterError::Closed)?;
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        self.shared.shutting_down.store(true, Ordering::Relaxed);
        self.shared.rpc.close();
        let child = self.child.locked().take();
        if let Some(mut child) = child {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        // The agent is gone or going; nothing it still sends matters. Without
        // a process to kill, the other end may never close, so stop reading.
        let reader = self.reader.locked().take();
        if let Some(reader) = reader {
            reader.abort();
            let _ = reader.await;
        }
        if !self.shared.projector().is_closed() {
            self.shared.on_closed();
        }
        Ok(())
    }
}

impl Drop for AcpSession {
    fn drop(&mut self) {
        self.shared.rpc.close();
        if let Some(reader) = self.reader.locked().take() {
            reader.abort();
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn option(id: &str, name: &str, kind: PermissionOptionKind) -> PermissionOption {
        PermissionOption { option_id: id.to_owned(), name: name.to_owned(), kind }
    }

    #[test]
    fn answers_pick_options_by_id_label_or_canonical_constant() {
        let options = [
            option("a1", "Allow", PermissionOptionKind::AllowOnce),
            option("a2", "Always allow", PermissionOptionKind::AllowAlways),
            option("r2", "Never", PermissionOptionKind::RejectAlways),
        ];
        assert_eq!(pick_option(&options, "a2").unwrap().option_id, "a2");
        assert_eq!(pick_option(&options, "Allow").unwrap().option_id, "a1");
        assert_eq!(pick_option(&options, answer::ALLOW_SESSION).unwrap().option_id, "a2");
        // No reject_once on offer: deny falls back to reject_always.
        assert_eq!(pick_option(&options, answer::DENY).unwrap().option_id, "r2");
        assert!(pick_option(&options, "maybe").is_none());
    }

    #[test]
    fn attachment_uris_are_percent_encoded() {
        assert_eq!(file_uri(std::path::Path::new("/tmp/my file#1.png")), "file:///tmp/my%20file%231.png");
    }
}
