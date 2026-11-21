//! The `surface_*` agent tools: one [`Tool`] each, on a [`ToolSet`] per scope.

use crate::browser::Tab;
use crate::docs::DataQuery;
use crate::hub::service::{Opened, SurfaceHub};
use crate::pixels::png_size;
use async_trait::async_trait;
use loa_contracts::harness::{HostToolSpec, HostTools, OutputBlock};
use loa_contracts::surface::{Input, InputEvent, Modifiers, MouseButton, SurfaceMode, SurfaceUri};
use loa_contracts::tools::{CallCtx, Tool, ToolOutput, ToolSet};
use loa_util::time::now_us;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub const LIST: &str = "surface_list";
pub const SCREENSHOT: &str = "surface_screenshot";
pub const CLICK: &str = "surface_click";
pub const TYPE: &str = "surface_type";
pub const SCROLL: &str = "surface_scroll";
pub const OPEN_BROWSER: &str = "surface_open_browser";
pub const NAVIGATE: &str = "surface_navigate";
pub const PAGE_TEXT: &str = "surface_page_text";
pub const ELEMENTS: &str = "surface_elements";
pub const DOCUMENT_DATA: &str = "surface_document_data";

fn err(e: impl Display) -> String {
    e.to_string()
}

fn pretty<T: serde::Serialize>(value: &T) -> Result<ToolOutput, String> {
    serde_json::to_string_pretty(value).map(ToolOutput::text).map_err(err)
}

/// What every tool works on: the hub, the built-in server it is limited to,
/// and the agent's input numbering (one more input source of the surface).
#[derive(Clone)]
struct Ctx {
    hub: Arc<SurfaceHub>,
    scope: Option<&'static str>,
    seq: Arc<AtomicU64>,
}

impl Ctx {
    fn uri(&self, uri: &str) -> Result<SurfaceUri, String> {
        let uri: SurfaceUri = uri.parse().map_err(err)?;
        match self.scope {
            Some(scope) if uri.server() != scope => Err(format!("{uri} isn't a {scope} surface")),
            _ => Ok(uri),
        }
    }

    async fn opened(&self, uri: &str) -> Result<Opened, String> {
        self.hub.open(&self.uri(uri)?).await.map_err(err)
    }

    /// A surface that takes input; documents are refused.
    async fn pixel(&self, uri: &str) -> Result<Opened, String> {
        let opened = self.opened(uri).await?;
        if opened.info.mode == SurfaceMode::Document {
            return Err(format!("{} is a document; read it with {DOCUMENT_DATA} or {SCREENSHOT}", opened.info.uri));
        }
        Ok(opened)
    }

    async fn tab(&self, uri: &str) -> Result<Arc<Tab>, String> {
        self.hub.tab(&self.uri(uri)?).await.map_err(err)
    }

    async fn input(&self, opened: &Opened, inputs: impl IntoIterator<Item = Input>) -> Result<(), String> {
        for input in inputs {
            let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
            opened.surface.input(InputEvent { seq, sent_us: now_us(), input }).await.map_err(err)?;
        }
        Ok(())
    }
}

/// Declares a tool on [`Ctx`]: `tool!(Type, NAME, Args, "description", |ctx, args| body)`.
macro_rules! tool {
    ($ty:ident, $name:expr, $args:ty, $desc:literal, |$c:ident, $a:ident| $body:block) => {
        struct $ty(Ctx);

        #[async_trait]
        impl Tool for $ty {
            type Args = $args;
            const NAME: &'static str = $name;
            fn description(&self) -> &str {
                $desc
            }
            async fn run(&self, _: &CallCtx, $a: $args) -> Result<ToolOutput, String> {
                let $c = &self.0;
                $body
            }
        }
    };
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UriArgs {
    /// The surface, e.g. surface://browser/tab1
    uri: String,
}

#[derive(Copy, Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum ClickButton {
    #[default]
    Left,
    Middle,
    Right,
}

#[derive(Copy, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum ModifierKey {
    Shift,
    Ctrl,
    Alt,
    Meta,
}

fn one() -> u32 {
    1
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ClickArgs {
    /// The surface, e.g. surface://browser/tab1
    uri: String,
    /// Pixels from the left edge of the surface
    x: f32,
    /// Pixels from the top edge of the surface
    y: f32,
    #[serde(default)]
    button: ClickButton,
    /// 2 for a double click, 3 for a triple click
    #[serde(default = "one")]
    #[schemars(range(min = 1, max = 3))]
    count: u32,
    /// Keys held during the click
    #[serde(default)]
    modifiers: Vec<ModifierKey>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TypeArgs {
    /// The surface, e.g. surface://browser/tab1
    uri: String,
    text: String,
    /// Press Enter after the text
    #[serde(default)]
    submit: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ScrollArgs {
    /// The surface, e.g. surface://browser/tab1
    uri: String,
    x: f32,
    y: f32,
    /// Pixels to scroll right
    #[serde(default)]
    dx: f32,
    /// Pixels to scroll down
    #[serde(default)]
    dy: f32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UrlArgs {
    url: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NavigateArgs {
    /// The surface, e.g. surface://browser/tab1
    uri: String,
    url: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DataArgs {
    /// The surface, e.g. surface://docs/1
    uri: String,
    query: DataQuery,
}

tool!(List, LIST, NoArgs, "List the surfaces open in the app (browser tabs, documents, VMs) with their uri, size and mode.", |c, _a| {
    let listed = c.hub.list().await;
    let rows: Vec<Value> = listed
        .iter()
        .filter(|l| c.scope.is_none_or(|s| l.info.uri.server() == s))
        .map(|l| {
            let i = &l.info;
            let server = l.origin.server();
            json!({ "uri": i.uri.as_str(), "name": i.name, "width": i.width, "height": i.height,
                    "mode": i.mode.as_str(), "mime": i.mime, "server": server, "open": l.open })
        })
        .collect();
    pretty(&rows)
});

tool!(Screenshot, SCREENSHOT, UriArgs, "Capture the surface as a PNG image.", |c, a| {
    let png = c.opened(&a.uri).await?.surface.screenshot().await.map_err(err)?;
    Ok(ToolOutput { blocks: vec![OutputBlock::Image { mime: "image/png".into(), data: png }], is_error: false })
});

tool!(Click, CLICK, ClickArgs, "Click at a point of the surface, in surface pixels.", |c, a| {
    if !(1..=3).contains(&a.count) {
        return Err(format!("bad arguments for {CLICK}: count must be 1, 2 or 3"));
    }
    let opened = c.pixel(&a.uri).await?;
    let has = |k| a.modifiers.contains(&k);
    let modifiers = Modifiers { shift: has(ModifierKey::Shift), ctrl: has(ModifierKey::Ctrl), alt: has(ModifierKey::Alt), meta: has(ModifierKey::Meta) };
    let button = match a.button {
        ClickButton::Left => MouseButton::Left,
        ClickButton::Middle => MouseButton::Middle,
        ClickButton::Right => MouseButton::Right,
    };
    let presses = (0..a.count).flat_map(|_| [true, false]);
    let clicks = presses.map(|pressed| Input::MouseButton { x: a.x, y: a.y, button, pressed, modifiers });
    c.input(&opened, std::iter::once(Input::MouseMove { x: a.x, y: a.y }).chain(clicks)).await?;
    Ok(ToolOutput::text(format!("Clicked ({}, {}) on {}", a.x, a.y, opened.info.uri)))
});

tool!(Type, TYPE, TypeArgs, "Type text into the focused element of the surface.", |c, a| {
    let opened = c.pixel(&a.uri).await?;
    let enter = |pressed| Input::Key { key: "Enter".into(), code: "Enter".into(), pressed, modifiers: Modifiers::default() };
    let submit = if a.submit { vec![enter(true), enter(false)] } else { Vec::new() };
    c.input(&opened, std::iter::once(Input::Text { text: a.text.clone() }).chain(submit)).await?;
    Ok(ToolOutput::text(format!("Typed {} characters on {}", a.text.chars().count(), opened.info.uri)))
});

tool!(Scroll, SCROLL, ScrollArgs, "Scroll at a point of the surface. Positive dy scrolls the content down.", |c, a| {
    let opened = c.pixel(&a.uri).await?;
    c.input(&opened, [Input::Scroll { x: a.x, y: a.y, dx: a.dx, dy: a.dy, modifiers: Modifiers::default() }]).await?;
    Ok(ToolOutput::text(format!("Scrolled ({}, {}) at ({}, {}) on {}", a.dx, a.dy, a.x, a.y, opened.info.uri)))
});

tool!(OpenBrowser, OPEN_BROWSER, UrlArgs, "Open a new browser tab at a URL. The user sees it in the Surfaces panel. Returns its surface uri.", |c, a| {
    let opened = c.hub.open_browser(&a.url).await.map_err(err)?;
    let title = opened.surface.info().name;
    pretty(&json!({ "uri": opened.info.uri.as_str(), "title": title, "width": opened.info.width, "height": opened.info.height }))
});

tool!(Navigate, NAVIGATE, NavigateArgs, "Load a URL in a browser surface and wait for it. Returns the new URL and title.", |c, a| {
    let tab = c.tab(&a.uri).await?;
    pretty(&tab.navigate(&crate::hub::normalize_url(&a.url)).await.map_err(err)?)
});

tool!(PageText, PAGE_TEXT, UriArgs, "The URL, title and readable text of the page in a browser surface.", |c, a| {
    let tab = c.tab(&a.uri).await?;
    let info = tab.page_info().await.map_err(err)?;
    let text = tab.page_text().await.map_err(err)?;
    Ok(ToolOutput::text(format!("URL: {}\nTitle: {}\n\n{text}", info.url, info.title)))
});

tool!(Elements, ELEMENTS, UriArgs, "The interactive elements of the page in a browser surface (role, name and box in surface pixels), to aim clicks.", |c, a| {
    pretty(&c.tab(&a.uri).await?.outline().await.map_err(err)?)
});

tool!(DocumentData, DOCUMENT_DATA, DataArgs, "Structured data of a document surface. query.kind is one of: info (any file), text (PDF pages, DOCX, PPTX slides; optional page from 0), sheets (XLSX), cells (XLSX; range in A1 notation, optional sheet), outline (DOCX), slides (PPTX).", |c, a| {
    let document = c.hub.document(&c.uri(&a.uri)?).map_err(err)?;
    pretty(&document.data(a.query).await.map_err(err)?)
});

/// The tools served for `scope` (a built-in server name), or every tool for `None`.
pub fn tool_set(hub: &Arc<SurfaceHub>, scope: Option<&'static str>) -> ToolSet {
    let c = Ctx { hub: hub.clone(), scope, seq: Arc::default() };
    let within = |server: &str| scope.is_none_or(|s| s == server);
    let mut set = ToolSet::new();
    set.register(List(c.clone())).register(Screenshot(c.clone()));
    if scope != Some(crate::docs::SERVER_NAME) {
        set.register(Click(c.clone())).register(Type(c.clone())).register(Scroll(c.clone()));
    }
    if within(crate::browser::SERVER_NAME) {
        set.register(OpenBrowser(c.clone())).register(Navigate(c.clone()));
        set.register(PageText(c.clone())).register(Elements(c.clone()));
    }
    if within(crate::docs::SERVER_NAME) {
        set.register(DocumentData(c));
    }
    set
}

/// The surface tools over a [`SurfaceHub`], as host tools.
pub struct SurfaceTools {
    hub: Arc<SurfaceHub>,
    set: ToolSet,
    shots: AtomicU64,
}

impl SurfaceTools {
    pub fn new(hub: Arc<SurfaceHub>) -> Arc<Self> {
        Arc::new(Self { set: tool_set(&hub, None), hub, shots: AtomicU64::new(0) })
    }

    pub fn hub(&self) -> &Arc<SurfaceHub> {
        &self.hub
    }

    /// The tools as host tools, with the handler that runs them.
    pub fn host_tools(self: &Arc<Self>) -> (Vec<HostToolSpec>, Arc<dyn HostTools>) {
        (self.set.specs(), self.clone())
    }

    /// Runs tool `name`; with a `scope` only that built-in server's tools and surfaces are reachable.
    pub async fn run(&self, name: &str, args: Value, scope: Option<&'static str>) -> ToolOutput {
        let call = CallCtx::default();
        let result = match scope {
            None => self.set.call(&call, name, args).await,
            Some(_) => tool_set(&self.hub, scope).call(&call, name, args).await,
        };
        result.unwrap_or_else(ToolOutput::error)
    }

    /// Saves a screenshot for a text-only result.
    fn save_screenshot(&self, png: &[u8]) -> Result<PathBuf, String> {
        let dir = &self.hub.config().screenshot_dir;
        let io = |e: std::io::Error| format!("saving the screenshot: {e}");
        std::fs::create_dir_all(dir).map_err(io)?;
        let n = self.shots.fetch_add(1, Ordering::Relaxed) + 1;
        let path = dir.join(format!("surface-{}-{n}.png", std::process::id()));
        std::fs::write(&path, png).map_err(io)?;
        Ok(path)
    }
}

#[async_trait]
impl HostTools for SurfaceTools {
    /// Text-only result: screenshots are saved as PNG files and their path returned.
    async fn call(&self, session_id: &str, tool: &str, args: &str) -> Result<String, String> {
        let blocks = self.call_content(session_id, tool, args).await?;
        let lines = blocks.into_iter().map(|block| match block {
            OutputBlock::Text(text) => Ok(text),
            OutputBlock::Image { data, .. } => {
                let path = self.save_screenshot(&data)?;
                let size = png_size(&data).map(|(w, h)| format!(" ({w}x{h})")).unwrap_or_default();
                Ok(format!("Screenshot{size} saved to {}", path.display()))
            }
        });
        Ok(lines.collect::<Result<Vec<_>, String>>()?.join("\n"))
    }

    async fn call_content(&self, session_id: &str, tool: &str, args: &str) -> Result<Vec<OutputBlock>, String> {
        HostTools::call_content(&self.set, session_id, tool, args).await
    }
}
