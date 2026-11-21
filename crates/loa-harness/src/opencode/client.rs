//! HTTP client of the OpenCode server (`opencode serve`).
//!
//! Every request carries `x-opencode-directory`, which selects the project
//! instance on the server, and HTTP basic auth when the server has a password
//! (`OPENCODE_SERVER_PASSWORD`).

use super::event::{Event, SseDecoder};
use super::native::WithParts;
use loa_contracts::harness::{AdapterError, Result};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde_json::{Value as Json, json};
use std::collections::VecDeque;
use std::time::Duration;

/// Default user name of the server's basic auth.
pub const DEFAULT_USERNAME: &str = "opencode";

/// Where a server is and how to log in.
#[derive(Clone, Debug)]
pub struct ServerAuth {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    directory: String,
    auth: Option<ServerAuth>,
}

impl Client {
    /// A client for the server at `base` (`http://127.0.0.1:4096`) working in
    /// `directory`.
    pub fn new(base: &str, directory: &str, auth: Option<ServerAuth>) -> Self {
        Client {
            http: reqwest::Client::new(),
            base: base.trim_end_matches('/').to_owned(),
            directory: directory.to_owned(),
            auth,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let req = self.http.request(method, format!("{}{path}", self.base)).header("x-opencode-directory", &self.directory);
        match &self.auth {
            Some(a) => req.basic_auth(&a.username, Some(&a.password)),
            None => req,
        }
    }

    async fn check(resp: std::result::Result<Response, reqwest::Error>, what: &str) -> Result<Response> {
        let resp = resp.map_err(|e| AdapterError::Protocol(format!("{what}: {e}")))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        Err(AdapterError::Protocol(format!("{what}: HTTP {status}: {body}")))
    }

    async fn call(&self, method: Method, path: &str, body: Option<Json>, what: &str) -> Result<Response> {
        let mut req = self.request(method, path);
        if let Some(body) = body {
            req = req.json(&body);
        }
        Self::check(req.send().await, what).await
    }

    async fn call_json<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Json>,
        what: &str,
    ) -> Result<T> {
        let resp = self.call(method, path, body, what).await?;
        resp.json().await.map_err(|e| AdapterError::Protocol(format!("{what}: {e}")))
    }

    /// `POST /session`. Returns the session info.
    pub async fn create_session(&self, body: Json) -> Result<Json> {
        self.call_json(Method::POST, "/session", Some(body), "create session").await
    }

    /// `GET /session/{id}`.
    pub async fn session(&self, id: &str) -> Result<Json> {
        self.call_json(Method::GET, &format!("/session/{id}"), None, "get session").await
    }

    /// `GET /session/status`: status of every busy session, by id.
    pub async fn statuses(&self) -> Result<serde_json::Map<String, Json>> {
        self.call_json(Method::GET, "/session/status", None, "session status").await
    }

    /// `GET /session/{id}/message`: every message with its parts, oldest first.
    pub async fn messages(&self, id: &str) -> Result<Vec<WithParts>> {
        self.call_json(Method::GET, &format!("/session/{id}/message"), None, "list messages").await
    }

    /// `POST /session/{id}/prompt_async`: starts a turn and returns at once.
    pub async fn prompt_async(&self, id: &str, body: Json) -> Result<()> {
        self.call(Method::POST, &format!("/session/{id}/prompt_async"), Some(body), "prompt").await.map(drop)
    }

    /// `POST /session/{id}/abort`.
    pub async fn abort(&self, id: &str) -> Result<()> {
        self.call(Method::POST, &format!("/session/{id}/abort"), None, "abort").await.map(drop)
    }

    /// `POST /session/{id}/fork`: copies the messages before `message_id`
    /// (all of them when `None`) into a new session.
    pub async fn fork(&self, id: &str, message_id: Option<&str>) -> Result<Json> {
        let body = match message_id {
            Some(m) => json!({ "messageID": m }),
            None => json!({}),
        };
        self.call_json(Method::POST, &format!("/session/{id}/fork"), Some(body), "fork").await
    }

    /// `POST /session/{id}/revert`: hides `message_id` and everything after
    /// it and restores the files.
    pub async fn revert(&self, id: &str, message_id: &str) -> Result<Json> {
        let body = json!({ "messageID": message_id });
        self.call_json(Method::POST, &format!("/session/{id}/revert"), Some(body), "revert").await
    }

    /// `POST /session/{id}/unrevert`.
    pub async fn unrevert(&self, id: &str) -> Result<Json> {
        self.call_json(Method::POST, &format!("/session/{id}/unrevert"), None, "unrevert").await
    }

    /// `POST /session/{id}/summarize`: compacts with the given model.
    pub async fn summarize(&self, id: &str, provider: &str, model: &str) -> Result<()> {
        let body = json!({ "providerID": provider, "modelID": model });
        self.call(Method::POST, &format!("/session/{id}/summarize"), Some(body), "summarize").await.map(drop)
    }

    /// `POST /permission/{request}/reply` with `once`, `always` or `reject`.
    pub async fn permission_reply(&self, request: &str, reply: &str, message: Option<&str>) -> Result<()> {
        let mut body = json!({ "reply": reply });
        if let Some(m) = message {
            body["message"] = json!(m);
        }
        self.call(Method::POST, &format!("/permission/{request}/reply"), Some(body), "permission reply").await.map(drop)
    }

    /// `POST /question/{request}/reply`: one list of picked labels or typed
    /// text per question.
    pub async fn question_reply(&self, request: &str, answers: &[Vec<String>]) -> Result<()> {
        let body = json!({ "answers": answers });
        self.call(Method::POST, &format!("/question/{request}/reply"), Some(body), "question reply").await.map(drop)
    }

    /// `POST /question/{request}/reject`.
    pub async fn question_reject(&self, request: &str) -> Result<()> {
        self.call(Method::POST, &format!("/question/{request}/reject"), None, "question reject").await.map(drop)
    }

    /// `POST /mcp`: connects an MCP server for this directory.
    pub async fn add_mcp(&self, name: &str, config: Json) -> Result<()> {
        let body = json!({ "name": name, "config": config });
        self.call(Method::POST, "/mcp", Some(body), "add MCP server").await.map(drop)
    }

    /// `GET /config/providers`: the connected providers with their models and
    /// the default model of each.
    pub async fn providers(&self) -> Result<Json> {
        self.call_json(Method::GET, "/config/providers", None, "list providers").await
    }

    /// `GET path` within `timeout`, for probing: the status and the JSON body
    /// (`null` when it isn't JSON). `Err` when no server answered.
    pub async fn probe(&self, path: &str, timeout: Duration) -> std::result::Result<(StatusCode, Json), reqwest::Error> {
        let resp = self.request(Method::GET, path).timeout(timeout).send().await?;
        let status = resp.status();
        Ok((status, resp.json().await.unwrap_or(Json::Null)))
    }

    /// `GET /event`: the server sent event stream of this directory.
    pub async fn events(&self) -> Result<EventStream> {
        let resp = self
            .call(Method::GET, "/event", None, "subscribe to events")
            .await?;
        Ok(EventStream { resp, decoder: SseDecoder::new(), ready: VecDeque::new() })
    }
}

/// The open `/event` stream.
pub struct EventStream {
    resp: Response,
    decoder: SseDecoder,
    ready: VecDeque<String>,
}

impl EventStream {
    /// The next event, `None` when the server closed the stream. Payloads
    /// that don't decode are skipped with a warning.
    pub async fn next(&mut self) -> Result<Option<Event>> {
        loop {
            while let Some(data) = self.ready.pop_front() {
                match Event::parse(&data) {
                    Ok(event) => return Ok(Some(event)),
                    Err(e) => tracing::warn!("undecodable OpenCode event: {e}: {data}"),
                }
            }
            match self.resp.chunk().await {
                Ok(Some(chunk)) => self.decoder.feed(&chunk, &mut self.ready),
                Ok(None) => return Ok(None),
                Err(e) => return Err(AdapterError::Protocol(format!("event stream: {e}"))),
            }
        }
    }
}
