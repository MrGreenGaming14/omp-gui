//! The `surface_*` agent tools over the hub's surfaces.
//!
//! Every surface server offers `surface_screenshot` and, for pixel surfaces,
//! `surface_click`, `surface_type` and `surface_scroll` (the shapes of
//! [`crate::mcp`]). On top of those each server offers the
//! structured data it has: the browser its URL, page text and interactive
//! elements, documents their text, cells, outline and slides.

use crate::hub::{Listed, SurfaceHub};
use crate::{HubError, Result};
use async_trait::async_trait;
use bytes::Bytes;
use loa_contracts::harness::{OutputBlock, HostToolSpec, HostTools};
use loa_contracts::surface::{SurfaceMode, SurfaceUri};
use loa_surface_docs::DataQuery;
use crate::mcp::{self as proto, ToolCall, ToolRunner};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub const LIST: &str = "surface_list";
pub const OPEN_BROWSER: &str = "surface_open_browser";
pub const NAVIGATE: &str = "surface_navigate";
pub const PAGE_TEXT: &str = "surface_page_text";
pub const ELEMENTS: &str = "surface_elements";
pub const DOCUMENT_DATA: &str = "surface_document_data";

/// One block of a tool result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolContent {
    Text(String),
    /// Raw image bytes and their MIME type.
    Image { mime: String, data: Bytes },
}

/// The outcome of a tool call; `is_error` results are shown to the model as
/// tool errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolResult {
    pub content: Vec<ToolContent>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self { content: vec![ToolContent::Text(text.into())], is_error: false }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self { content: vec![ToolContent::Text(text.into())], is_error: true }
    }

    fn json<T: serde::Serialize>(value: &T) -> Self {
        match serde_json::to_string_pretty(value) {
            Ok(text) => Self::text(text),
            Err(e) => Self::error(e.to_string()),
        }
    }

    /// An MCP `CallToolResult`.
    pub fn to_mcp(&self) -> Value {
        use base64::Engine;
        let content: Vec<Value> = self
            .content
            .iter()
            .map(|block| match block {
                ToolContent::Text(text) => json!({ "type": "text", "text": text }),
                ToolContent::Image { mime, data } => json!({
                    "type": "image",
                    "mimeType": mime,
                    "data": base64::engine::general_purpose::STANDARD.encode(data),
                }),
            })
            .collect();
        let mut result = json!({ "content": content });
        if self.is_error {
            result["isError"] = json!(true);
        }
        result
    }

    /// Reads an MCP `CallToolResult`. Blocks other than text and images are
    /// passed on as their JSON text.
    pub fn from_mcp(result: &Value) -> Self {
        use base64::Engine;
        let blocks = result.get("content").and_then(Value::as_array).cloned().unwrap_or_default();
        let content = blocks
            .iter()
            .map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => ToolContent::Text(block.get("text").and_then(Value::as_str).unwrap_or_default().to_owned()),
                Some("image") => {
                    let data = block.get("data").and_then(Value::as_str).unwrap_or_default();
                    match base64::engine::general_purpose::STANDARD.decode(data) {
                        Ok(bytes) => ToolContent::Image {
                            mime: block.get("mimeType").and_then(Value::as_str).unwrap_or("image/png").to_owned(),
                            data: Bytes::from(bytes),
                        },
                        Err(e) => ToolContent::Text(format!("undecodable image: {e}")),
                    }
                }
                _ => ToolContent::Text(block.to_string()),
            })
            .collect();
        Self { content, is_error: result.get("isError").and_then(Value::as_bool).unwrap_or(false) }
    }

    /// The first image block.
    pub fn image(&self) -> Option<&Bytes> {
        self.content.iter().find_map(|c| match c {
            ToolContent::Image { data, .. } => Some(data),
            ToolContent::Text(_) => None,
        })
    }

    /// The text blocks joined by newlines.
    pub fn joined_text(&self) -> String {
        let texts: Vec<&str> = self
            .content
            .iter()
            .filter_map(|c| match c {
                ToolContent::Text(text) => Some(text.as_str()),
                ToolContent::Image { .. } => None,
            })
            .collect();
        texts.join("\n")
    }
}

fn uri_schema() -> Value {
    json!({ "type": "string", "description": "The surface, e.g. surface://browser/tab1" })
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

/// MCP tool definitions of the tools served for `scope` (a built-in server
/// name: `browser`, `docs` or `vm`), or of every tool for `None`.
pub fn tool_definitions(scope: Option<&str>) -> Vec<Value> {
    let browser = scope.is_none_or(|s| s == loa_surface_browser::SERVER_NAME);
    let docs = scope.is_none_or(|s| s == loa_surface_docs::SERVER_NAME);
    let pixel = scope.is_none_or(|s| s != loa_surface_docs::SERVER_NAME);
    let mut tools = vec![json!({
        "name": LIST,
        "description": "List the surfaces open in the app (browser tabs, documents, VMs) with their uri, size and mode.",
        "inputSchema": object(json!({}), &[]),
    })];
    tools.extend(proto::tools().into_iter().filter(|t| pixel || t["name"] == proto::SCREENSHOT));
    if browser {
        tools.push(json!({
            "name": OPEN_BROWSER,
            "description": "Open a new browser tab at a URL. The user sees it in the Surfaces panel. Returns its surface uri.",
            "inputSchema": object(json!({ "url": { "type": "string" } }), &["url"]),
        }));
        tools.push(json!({
            "name": NAVIGATE,
            "description": "Load a URL in a browser surface and wait for it. Returns the new URL and title.",
            "inputSchema": object(json!({ "uri": uri_schema(), "url": { "type": "string" } }), &["uri", "url"]),
        }));
        tools.push(json!({
            "name": PAGE_TEXT,
            "description": "The URL, title and readable text of the page in a browser surface.",
            "inputSchema": object(json!({ "uri": uri_schema() }), &["uri"]),
        }));
        tools.push(json!({
            "name": ELEMENTS,
            "description": "The interactive elements of the page in a browser surface (role, name and box in surface pixels), to aim clicks.",
            "inputSchema": object(json!({ "uri": uri_schema() }), &["uri"]),
        }));
    }
    if docs {
        tools.push(json!({
            "name": DOCUMENT_DATA,
            "description": "Structured data of a document surface. query.kind is one of: info (any file), text (PDF pages, DOCX, PPTX slides; optional page from 0), sheets (XLSX), cells (XLSX; range in A1 notation, optional sheet), outline (DOCX), slides (PPTX).",
            "inputSchema": object(json!({
                "uri": uri_schema(),
                "query": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["info", "text", "sheets", "cells", "outline", "slides"] },
                        "page": { "type": "integer", "minimum": 0 },
                        "sheet": { "type": "string" },
                        "range": { "type": "string" },
                    },
                    "required": ["kind"],
                },
            }), &["uri", "query"]),
        }));
    }
    tools
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UrlArgs {
    url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NavigateArgs {
    uri: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UriArgs {
    uri: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DataArgs {
    uri: String,
    query: DataQuery,
}

fn parse<'de, T: Deserialize<'de>>(tool: &str, args: &'de Value) -> Result<T, String> {
    T::deserialize(args).map_err(|e| format!("bad arguments for {tool}: {e}"))
}

/// Runs the surface tools on a [`SurfaceHub`].
pub struct SurfaceTools {
    hub: Arc<SurfaceHub>,
    runner: ToolRunner,
    shots: AtomicU64,
}

impl SurfaceTools {
    pub fn new(hub: Arc<SurfaceHub>) -> Arc<Self> {
        Arc::new(Self { hub, runner: ToolRunner::new(), shots: AtomicU64::new(0) })
    }

    pub fn hub(&self) -> &Arc<SurfaceHub> {
        &self.hub
    }

    /// The tools as host tools, with the handler that runs them, for the
    /// app's host tool router.
    pub fn host_tools(self: &Arc<Self>) -> (Vec<HostToolSpec>, Arc<dyn HostTools>) {
        let specs = tool_definitions(None)
            .into_iter()
            .map(|tool| HostToolSpec {
                name: tool["name"].as_str().unwrap_or_default().to_owned(),
                description: tool["description"].as_str().unwrap_or_default().to_owned(),
                input_schema: tool["inputSchema"].to_string(),
            })
            .collect();
        (specs, self.clone())
    }

    /// Runs tool `name` with JSON `args`. With a `scope`, only that built-in
    /// server's surfaces are reachable.
    pub async fn run(&self, name: &str, args: &Value, scope: Option<&str>) -> ToolResult {
        match self.try_call(name, args, scope).await {
            Ok(result) => result,
            Err(message) => ToolResult::error(message),
        }
    }

    async fn try_call(&self, name: &str, args: &Value, scope: Option<&str>) -> Result<ToolResult, String> {
        let in_scope = |uri: &str| -> Result<SurfaceUri, String> {
            let uri: SurfaceUri = uri.parse().map_err(|e: loa_contracts::surface::SurfaceError| e.to_string())?;
            match scope {
                Some(scope) if uri.server() != scope => Err(format!("{uri} isn't a {scope} surface")),
                _ => Ok(uri),
            }
        };
        if let Some(call) = ToolCall::parse(name, args).map_err(|e| e.to_string())? {
            let uri = in_scope(call.uri())?;
            let opened = self.hub.open(&uri).await.map_err(|e| e.to_string())?;
            if let ToolCall::Screenshot(_) = call {
                let png = opened.surface.screenshot().await.map_err(|e| e.to_string())?;
                return Ok(ToolResult { content: vec![ToolContent::Image { mime: "image/png".into(), data: png }], is_error: false });
            }
            if opened.info.mode == SurfaceMode::Document {
                return Err(format!("{uri} is a document; read it with {DOCUMENT_DATA} or {}", proto::SCREENSHOT));
            }
            return Ok(ToolResult::from_mcp(&self.runner.run(&call, opened.surface.as_ref()).await));
        }
        let browser = scope.is_none_or(|s| s == loa_surface_browser::SERVER_NAME);
        let docs = scope.is_none_or(|s| s == loa_surface_docs::SERVER_NAME);
        match name {
            LIST => {
                let listed: Vec<Value> = self
                    .hub
                    .list()
                    .await
                    .into_iter()
                    .filter(|l| scope.is_none_or(|s| l.info.uri.server() == s))
                    .map(|l| listed_json(&l))
                    .collect();
                Ok(ToolResult::json(&listed))
            }
            OPEN_BROWSER if browser => {
                let args: UrlArgs = parse(name, args)?;
                let opened = self.hub.open_browser(&args.url).await.map_err(|e| e.to_string())?;
                Ok(ToolResult::json(&json!({
                    "uri": opened.info.uri.as_str(),
                    "title": opened.surface.info().name,
                    "width": opened.info.width,
                    "height": opened.info.height,
                })))
            }
            NAVIGATE if browser => {
                let args: NavigateArgs = parse(name, args)?;
                let tab = self.hub.tab(&in_scope(&args.uri)?).await.map_err(|e| e.to_string())?;
                let info = tab.navigate(&crate::normalize_url(&args.url)).await.map_err(|e| e.to_string())?;
                Ok(ToolResult::json(&info))
            }
            PAGE_TEXT if browser => {
                let args: UriArgs = parse(name, args)?;
                let tab = self.hub.tab(&in_scope(&args.uri)?).await.map_err(|e| e.to_string())?;
                let info = tab.page_info().await.map_err(|e| e.to_string())?;
                let text = tab.page_text().await.map_err(|e| e.to_string())?;
                Ok(ToolResult::text(format!("URL: {}\nTitle: {}\n\n{text}", info.url, info.title)))
            }
            ELEMENTS if browser => {
                let args: UriArgs = parse(name, args)?;
                let tab = self.hub.tab(&in_scope(&args.uri)?).await.map_err(|e| e.to_string())?;
                Ok(ToolResult::json(&tab.outline().await.map_err(|e| e.to_string())?))
            }
            DOCUMENT_DATA if docs => {
                let args: DataArgs = parse(name, args)?;
                let document = self.hub.document(&in_scope(&args.uri)?).map_err(|e| e.to_string())?;
                Ok(ToolResult::json(&document.data(args.query).await.map_err(|e| e.to_string())?))
            }
            other => Err(format!("no surface tool {other}")),
        }
    }

    /// Saves a screenshot for a text-only result and describes it.
    fn save_screenshot(&self, png: &[u8]) -> Result<PathBuf, HubError> {
        let dir = &self.hub.config().screenshot_dir;
        let io = |e: std::io::Error| HubError::Surface(loa_contracts::surface::SurfaceError::Backend(format!("saving the screenshot: {e}")));
        std::fs::create_dir_all(dir).map_err(io)?;
        let n = self.shots.fetch_add(1, Ordering::Relaxed) + 1;
        let path = dir.join(format!("surface-{}-{n}.png", std::process::id()));
        std::fs::write(&path, png).map_err(io)?;
        Ok(path)
    }
}

fn listed_json(listed: &Listed) -> Value {
    let info = &listed.info;
    json!({
        "uri": info.uri.as_str(),
        "name": info.name,
        "width": info.width,
        "height": info.height,
        "mode": info.mode.as_str(),
        "mime": info.mime,
        "server": listed.origin.server(),
        "open": listed.open,
    })
}

/// Width and height from a PNG header.
pub(crate) fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(png[20..24].try_into().ok()?);
    Some((width, height))
}

fn args_json(args: &str) -> Result<Value, String> {
    if args.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(args).map_err(|e| format!("arguments aren't JSON: {e}"))
}

#[async_trait]
impl HostTools for SurfaceTools {
    /// Text-only result: screenshots are saved as PNG files and their path
    /// is returned, so the model can read the image with its file tools.
    async fn call(&self, _session_id: &str, tool: &str, args: &str) -> Result<String, String> {
        let result = self.run(tool, &args_json(args)?, None).await;
        let mut lines = Vec::new();
        for block in &result.content {
            match block {
                ToolContent::Text(text) => lines.push(text.clone()),
                ToolContent::Image { data, .. } => {
                    let path = self.save_screenshot(data).map_err(|e| e.to_string())?;
                    let size = png_size(data).map(|(w, h)| format!(" ({w}x{h})")).unwrap_or_default();
                    lines.push(format!("Screenshot{size} saved to {}", path.display()));
                }
            }
        }
        let text = lines.join("\n");
        if result.is_error { Err(text) } else { Ok(text) }
    }

    async fn call_content(&self, _session_id: &str, tool: &str, args: &str) -> Result<Vec<OutputBlock>, String> {
        let result = self.run(tool, &args_json(args)?, None).await;
        if result.is_error {
            return Err(result.joined_text());
        }
        Ok(result
            .content
            .into_iter()
            .map(|block| match block {
                ToolContent::Text(text) => OutputBlock::Text(text),
                ToolContent::Image { mime, data } => OutputBlock::Image { mime, data },
            })
            .collect())
    }
}
