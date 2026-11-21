//! HTTP client of `opencode serve`. Every request carries `x-opencode-directory`
//! (the project instance) and, when the server has a password, basic auth.

use super::event::{Event, SseDecoder};
use loa_contracts::harness::{AdapterError, Result};
use reqwest::{Method, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value as Json;
use std::collections::VecDeque;
use std::time::Duration;

/// HTTP basic auth of a server.
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
    /// A client for the server at `base` (`http://127.0.0.1:4096`) working in `directory`.
    pub fn new(base: &str, directory: &str, auth: Option<ServerAuth>) -> Self {
        let base = base.trim_end_matches('/').to_owned();
        Client { http: reqwest::Client::new(), base, directory: directory.to_owned(), auth }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let req = self.http.request(method, format!("{}{path}", self.base)).header("x-opencode-directory", &self.directory);
        match &self.auth {
            Some(a) => req.basic_auth(&a.username, Some(&a.password)),
            None => req,
        }
    }

    /// `method path` with an optional JSON `body`; an error unless the server answers 2xx.
    pub async fn endpoint(&self, method: Method, path: &str, body: Option<Json>) -> Result<Response> {
        let what = format!("{method} {path}");
        let mut req = self.request(method, path);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.map_err(|e| AdapterError::Protocol(format!("{what}: {e}")))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        Err(AdapterError::Protocol(format!("{what}: HTTP {status}: {body}")))
    }

    /// [`Client::endpoint`] with its JSON answer decoded.
    pub async fn json<T: DeserializeOwned>(&self, method: Method, path: &str, body: Option<Json>) -> Result<T> {
        let resp = self.endpoint(method, path, body).await?;
        resp.json().await.map_err(|e| AdapterError::Protocol(format!("{path}: {e}")))
    }

    /// `GET path` within `timeout`: the status and the JSON body (`null` when not JSON). `Err` when no server answered.
    pub async fn probe(&self, path: &str, timeout: Duration) -> std::result::Result<(StatusCode, Json), reqwest::Error> {
        let resp = self.request(Method::GET, path).timeout(timeout).send().await?;
        let status = resp.status();
        Ok((status, resp.json().await.unwrap_or(Json::Null)))
    }

    /// `GET /event`: the event stream of this directory.
    pub async fn events(&self) -> Result<EventStream> {
        let resp = self.endpoint(Method::GET, "/event", None).await?;
        Ok(EventStream { resp, decoder: SseDecoder::default(), ready: VecDeque::new() })
    }
}

/// The open `/event` stream.
pub struct EventStream {
    resp: Response,
    decoder: SseDecoder,
    ready: VecDeque<String>,
}

impl EventStream {
    /// The next event, `None` once the server closed the stream. Undecodable payloads are skipped.
    pub async fn next(&mut self) -> Result<Option<Event>> {
        loop {
            while let Some(data) = self.ready.pop_front() {
                match serde_json::from_str(&data) {
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
