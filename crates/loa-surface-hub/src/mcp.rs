//! How a surface server declares its surfaces and agent tools through MCP.
//!
//! Plain `serde_json::Value`s in the shapes of the MCP spec (2025-06-18), so
//! any MCP library can carry them:
//!
//! - each surface is a resource whose `uri` is the `surface://` uri, whose
//!   `name` is the tab label, and whose `_meta["omp/surface"]` holds the
//!   pixel size and mode. Document surfaces also set `mimeType`
//! - every surface server exposes the tools in [`tools`] and runs calls with
//!   a [`ToolRunner`]

use base64::prelude::{BASE64_STANDARD, Engine as _};
use loa_contracts::surface::{Input, Modifiers, MouseButton, Surface, SurfaceInfo, SurfaceMode, SurfaceUri};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

use loa_util::time::now_us;

/// `_meta` key of the surface fields on a resource.
pub const META_KEY: &str = "omp/surface";

pub const SCREENSHOT: &str = "surface_screenshot";
pub const CLICK: &str = "surface_click";
pub const TYPE: &str = "surface_type";
pub const SCROLL: &str = "surface_scroll";

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("resource is not an object with `uri` and `name`: {0}")]
    Resource(serde_json::Error),
    #[error("surface resource {uri} lacks `_meta.\"omp/surface\"` with width, height and mode")]
    Meta { uri: String },
    #[error("unknown surface mode `{0}`")]
    Mode(String),
    #[error(transparent)]
    Uri(#[from] loa_contracts::surface::SurfaceError),
    #[error("bad arguments for {tool}: {source}")]
    Arguments { tool: &'static str, source: serde_json::Error },
}

pub type Result<T, E = McpError> = std::result::Result<T, E>;

/// The MCP resource that declares `info`.
pub fn resource(info: &SurfaceInfo) -> Value {
    let mut resource = json!({
        "uri": info.uri.as_str(),
        "name": info.name,
        "_meta": {
            META_KEY: {
                "width": info.width,
                "height": info.height,
                "mode": info.mode.as_str(),
            }
        }
    });
    if let Some(mime) = &info.mime {
        resource["mimeType"] = json!(mime);
    }
    resource
}

#[derive(Deserialize)]
struct ResourceJson {
    uri: String,
    name: String,
    #[serde(rename = "mimeType")]
    mime: Option<String>,
    #[serde(rename = "_meta")]
    meta: Option<serde_json::Map<String, Value>>,
}

#[derive(Deserialize)]
struct SurfaceMeta {
    width: u32,
    height: u32,
    mode: String,
}

/// The surface a resource declares. `Ok(None)` for resources that are not
/// `surface://` uris, so a client can run a whole server's resource list
/// through it.
pub fn parse_resource(resource: &Value) -> Result<Option<SurfaceInfo>> {
    let r = ResourceJson::deserialize(resource).map_err(McpError::Resource)?;
    if !r.uri.starts_with(&format!("{}://", loa_contracts::surface::SCHEME)) {
        return Ok(None);
    }
    let uri: SurfaceUri = r.uri.parse()?;
    let meta = r
        .meta
        .as_ref()
        .and_then(|m| m.get(META_KEY))
        .and_then(|m| SurfaceMeta::deserialize(m).ok())
        .ok_or_else(|| McpError::Meta { uri: r.uri.clone() })?;
    let mode = SurfaceMode::parse(&meta.mode).ok_or(McpError::Mode(meta.mode))?;
    Ok(Some(SurfaceInfo { uri, name: r.name, width: meta.width, height: meta.height, mode, mime: r.mime }))
}

/// MCP tool definitions (`name`, `description`, `inputSchema`) of the
/// agent tools every surface server exposes.
pub fn tools() -> Vec<Value> {
    let uri = json!({ "type": "string", "description": "The surface, e.g. surface://browser/tab1" });
    let x = json!({ "type": "number", "description": "Pixels from the left edge of the surface" });
    let y = json!({ "type": "number", "description": "Pixels from the top edge of the surface" });
    vec![
        json!({
            "name": SCREENSHOT,
            "description": "Capture the surface as a PNG image.",
            "inputSchema": {
                "type": "object",
                "properties": { "uri": uri },
                "required": ["uri"],
                "additionalProperties": false,
            }
        }),
        json!({
            "name": CLICK,
            "description": "Click at a point of the surface, in surface pixels.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "uri": uri,
                    "x": x,
                    "y": y,
                    "button": { "type": "string", "enum": ["left", "middle", "right"], "default": "left" },
                    "count": { "type": "integer", "minimum": 1, "maximum": 3, "default": 1,
                               "description": "2 for a double click, 3 for a triple click" },
                    "modifiers": { "type": "array", "items": { "type": "string", "enum": ["shift", "ctrl", "alt", "meta"] },
                                   "description": "Keys held during the click" },
                },
                "required": ["uri", "x", "y"],
                "additionalProperties": false,
            }
        }),
        json!({
            "name": TYPE,
            "description": "Type text into the focused element of the surface.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "uri": uri,
                    "text": { "type": "string" },
                    "submit": { "type": "boolean", "default": false, "description": "Press Enter after the text" },
                },
                "required": ["uri", "text"],
                "additionalProperties": false,
            }
        }),
        json!({
            "name": SCROLL,
            "description": "Scroll at a point of the surface. Positive dy scrolls the content down.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "uri": uri,
                    "x": x,
                    "y": y,
                    "dx": { "type": "number", "default": 0, "description": "Pixels to scroll right" },
                    "dy": { "type": "number", "default": 0, "description": "Pixels to scroll down" },
                },
                "required": ["uri", "x", "y"],
                "additionalProperties": false,
            }
        }),
    ]
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotArgs {
    pub uri: String,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClickButton {
    #[default]
    Left,
    Middle,
    Right,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModifierKey {
    Shift,
    Ctrl,
    Alt,
    Meta,
}

fn one() -> u32 {
    1
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClickArgs {
    pub uri: String,
    pub x: f32,
    pub y: f32,
    #[serde(default)]
    pub button: ClickButton,
    #[serde(default = "one")]
    pub count: u32,
    #[serde(default)]
    pub modifiers: Vec<ModifierKey>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeArgs {
    pub uri: String,
    pub text: String,
    #[serde(default)]
    pub submit: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScrollArgs {
    pub uri: String,
    pub x: f32,
    pub y: f32,
    #[serde(default)]
    pub dx: f32,
    #[serde(default)]
    pub dy: f32,
}

/// A parsed call of one of the surface tools.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolCall {
    Screenshot(ScreenshotArgs),
    Click(ClickArgs),
    Type(TypeArgs),
    Scroll(ScrollArgs),
}

fn args<'de, T: Deserialize<'de>>(tool: &'static str, value: &'de Value) -> Result<T> {
    T::deserialize(value).map_err(|source| McpError::Arguments { tool, source })
}

impl ToolCall {
    /// Parses a `tools/call`. `Ok(None)` for tools that are not surface
    /// tools, so a server can fall through to its own.
    pub fn parse(name: &str, arguments: &Value) -> Result<Option<Self>> {
        Ok(Some(match name {
            SCREENSHOT => Self::Screenshot(args(SCREENSHOT, arguments)?),
            CLICK => {
                let click: ClickArgs = args(CLICK, arguments)?;
                if !(1..=3).contains(&click.count) {
                    let source = serde::de::Error::custom("count must be 1, 2 or 3");
                    return Err(McpError::Arguments { tool: CLICK, source });
                }
                Self::Click(click)
            }
            TYPE => Self::Type(args(TYPE, arguments)?),
            SCROLL => Self::Scroll(args(SCROLL, arguments)?),
            _ => return Ok(None),
        }))
    }

    /// The surface the call is for; the server looks it up and passes it
    /// to [`ToolRunner::run`].
    pub fn uri(&self) -> &str {
        match self {
            Self::Screenshot(a) => &a.uri,
            Self::Click(a) => &a.uri,
            Self::Type(a) => &a.uri,
            Self::Scroll(a) => &a.uri,
        }
    }
}

/// Runs surface tool calls. The agent is one more input source of the
/// surface, so the runner numbers its input events like a viewer does.
#[derive(Debug)]
pub struct ToolRunner {
    next_seq: AtomicU64,
}

impl Default for ToolRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRunner {
    pub fn new() -> Self {
        Self { next_seq: AtomicU64::new(1) }
    }

    /// Runs `call` on `surface` and returns the MCP `CallToolResult`.
    /// Failures come back as a result with `isError`, as MCP wants for tool
    /// errors.
    pub async fn run(&self, call: &ToolCall, surface: &dyn Surface) -> Value {
        match self.try_run(call, surface).await {
            Ok(result) => result,
            Err(e) => json!({ "content": [{ "type": "text", "text": e.to_string() }], "isError": true }),
        }
    }

    async fn try_run(&self, call: &ToolCall, surface: &dyn Surface) -> loa_contracts::surface::Result<Value> {
        let uri = surface.info().uri;
        if call.uri() != uri.as_str() {
            return Err(loa_contracts::surface::SurfaceError::NotFound(call.uri().parse()?));
        }
        let text = match call {
            ToolCall::Screenshot(_) => {
                let png = surface.screenshot().await?;
                let data = BASE64_STANDARD.encode(&png);
                return Ok(json!({ "content": [{ "type": "image", "data": data, "mimeType": "image/png" }] }));
            }
            ToolCall::Click(a) => {
                let modifiers = modifiers(&a.modifiers);
                let button = match a.button {
                    ClickButton::Left => MouseButton::Left,
                    ClickButton::Middle => MouseButton::Middle,
                    ClickButton::Right => MouseButton::Right,
                };
                self.input(surface, Input::MouseMove { x: a.x, y: a.y }).await?;
                for _ in 0..a.count {
                    for pressed in [true, false] {
                        let input = Input::MouseButton { x: a.x, y: a.y, button, pressed, modifiers };
                        self.input(surface, input).await?;
                    }
                }
                format!("Clicked ({}, {}) on {uri}", a.x, a.y)
            }
            ToolCall::Type(a) => {
                self.input(surface, Input::Text { text: a.text.clone() }).await?;
                if a.submit {
                    for pressed in [true, false] {
                        let key = Input::Key {
                            key: "Enter".into(),
                            code: "Enter".into(),
                            pressed,
                            modifiers: Modifiers::default(),
                        };
                        self.input(surface, key).await?;
                    }
                }
                format!("Typed {} characters on {uri}", a.text.chars().count())
            }
            ToolCall::Scroll(a) => {
                let input = Input::Scroll { x: a.x, y: a.y, dx: a.dx, dy: a.dy, modifiers: Modifiers::default() };
                self.input(surface, input).await?;
                format!("Scrolled ({}, {}) at ({}, {}) on {uri}", a.dx, a.dy, a.x, a.y)
            }
        };
        Ok(json!({ "content": [{ "type": "text", "text": text }] }))
    }

    async fn input(&self, surface: &dyn Surface, input: Input) -> loa_contracts::surface::Result<()> {
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        surface.input(loa_contracts::surface::InputEvent { seq, sent_us: now_us(), input }).await
    }
}

fn modifiers(keys: &[ModifierKey]) -> Modifiers {
    Modifiers {
        shift: keys.contains(&ModifierKey::Shift),
        ctrl: keys.contains(&ModifierKey::Ctrl),
        alt: keys.contains(&ModifierKey::Alt),
        meta: keys.contains(&ModifierKey::Meta),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_resources_round_trip_and_bad_calls_are_refused() {
        let info = SurfaceInfo {
            uri: "surface://docs/a.pdf".parse().unwrap(),
            name: "a.pdf".into(),
            width: 64,
            height: 48,
            mode: SurfaceMode::Document,
            mime: Some("application/pdf".into()),
        };
        assert_eq!(parse_resource(&resource(&info)).unwrap(), Some(info));
        assert_eq!(parse_resource(&json!({ "uri": "file:///notes.txt", "name": "n" })).unwrap(), None);
        assert!(matches!(parse_resource(&json!({ "uri": "surface://t/x", "name": "x" })), Err(McpError::Meta { .. })));
        assert!(ToolCall::parse(CLICK, &json!({ "uri": "surface://t/a", "x": 1, "y": 1, "count": 9 })).is_err());
        assert!(ToolCall::parse(TYPE, &json!({ "uri": "surface://t/a", "text": "x", "extra": 1 })).is_err());
        assert_eq!(ToolCall::parse("browser_dom", &json!({})).unwrap(), None);
    }
}
