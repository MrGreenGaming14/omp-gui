//! A surface a third party MCP server declares, viewed through its own
//! `surface_*` tools.
//!
//! The frame and input stream (`omp/surface/1`) is not reachable through MCP
//! alone, so the picture comes from `surface_screenshot` (taken when a viewer
//! subscribes, after each input and twice a second while watched) and input
//! goes out as `surface_click`, `surface_type` and `surface_scroll` calls.
//! Mouse moves, drags and keys other than Enter have no tool and are not
//! sent. Document surfaces are read once with `resources/read`.

use crate::feed::{FrameFeed, Picture};
use crate::hub::tools::{CLICK, SCREENSHOT, SCROLL, TYPE};
use crate::pixels::png_size;
use async_trait::async_trait;
use bytes::Bytes;
use loa_contracts::surface::{
    Document, Encoding, Frame, Input, InputEvent, Modifiers, MouseButton, Rect, Surface, SurfaceError, SurfaceInfo,
    SurfaceMode,
};
use loa_contracts::tools::{OutputBlock, ToolOutput};
use loa_mcp::model::ResourceContents;
use loa_mcp::{AllowAll, Manager};
use loa_util::LockExt;
use loa_util::time::now_us;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, broadcast};
use tokio::task::JoinHandle;

/// Time between screenshots while a viewer watches.
const POLL: Duration = Duration::from_millis(500);

struct Shared {
    manager: Manager,
    server: String,
    info: Mutex<SurfaceInfo>,
    feed: FrameFeed,
    /// Highest input whose tool call returned.
    input_done: AtomicU64,
    wake: Notify,
    /// Left button press waiting for its release to become a click.
    press: Mutex<Option<MouseButton>>,
}

impl Shared {
    async fn call(&self, tool: &str, arguments: Value) -> loa_contracts::surface::Result<ToolOutput> {
        let arguments = if let Value::Object(map) = arguments { Some(map) } else { None };
        let result = self.manager.call_tool(&self.server, tool, arguments, &AllowAll).await;
        let output = loa_mcp::toolkit::to_output(result.map_err(|e| SurfaceError::Backend(e.to_string()))?);
        if output.is_error {
            return Err(SurfaceError::Backend(format!("{tool} on {}: {}", self.server, output.joined_text())));
        }
        Ok(output)
    }

    fn uri(&self) -> String {
        self.info.locked().uri.as_str().to_owned()
    }

    async fn screenshot(&self) -> loa_contracts::surface::Result<Bytes> {
        let output = self.call(SCREENSHOT, json!({ "uri": self.uri() })).await?;
        let image = output.blocks.into_iter().find_map(|b| if let OutputBlock::Image { data, .. } = b { Some(data) } else { None });
        image.ok_or_else(|| SurfaceError::Backend(format!("{SCREENSHOT} returned no image")))
    }

    /// Takes a screenshot and publishes it as a PNG keyframe.
    async fn publish(&self) -> loa_contracts::surface::Result<()> {
        let input_seq = self.input_done.load(Ordering::Acquire);
        let png = self.screenshot().await?;
        let (width, height) = png_size(&png).ok_or_else(|| SurfaceError::Backend(format!("{SCREENSHOT} isn't a PNG")))?;
        {
            let mut info = self.info.locked();
            (info.width, info.height) = (width, height);
        }
        self.feed.publish(Picture {
            captured_us: now_us(),
            input_seq,
            width,
            height,
            region: Rect { x: 0, y: 0, width, height },
            encoding: Encoding::Png,
            keyframe: true,
            data: png,
        });
        Ok(())
    }

    async fn input(&self, event: &InputEvent) -> loa_contracts::surface::Result<()> {
        let uri = self.uri();
        let button_name = |button: MouseButton| match button {
            MouseButton::Left => Ok("left"),
            MouseButton::Middle => Ok("middle"),
            MouseButton::Right => Ok("right"),
            MouseButton::Back | MouseButton::Forward => Err(SurfaceError::Backend(format!("{CLICK} takes left, middle and right clicks"))),
        };
        let modifier_names = |m: &Modifiers| {
            let all = [(m.shift, "shift"), (m.ctrl, "ctrl"), (m.alt, "alt"), (m.meta, "meta")];
            all.into_iter().filter_map(|(on, name)| on.then_some(name)).collect::<Vec<_>>()
        };
        match &event.input {
            Input::MouseButton { button, pressed: true, .. } => {
                *self.press.locked() = Some(*button);
                return Ok(());
            }
            Input::MouseButton { x, y, button, pressed: false, modifiers } => {
                let pressed = self.press.locked().take();
                if pressed != Some(*button) {
                    return Ok(());
                }
                let args = json!({ "uri": uri, "x": x, "y": y, "button": button_name(*button)?, "modifiers": modifier_names(modifiers) });
                self.call(CLICK, args).await?;
            }
            Input::Scroll { x, y, dx, dy, .. } => {
                self.call(SCROLL, json!({ "uri": uri, "x": x, "y": y, "dx": dx, "dy": dy })).await?;
            }
            Input::Text { text } => {
                self.call(TYPE, json!({ "uri": uri, "text": text })).await?;
            }
            Input::Key { key, pressed: true, .. } if key == "Enter" => {
                self.call(TYPE, json!({ "uri": uri, "text": "", "submit": true })).await?;
            }
            Input::Key { .. } | Input::MouseMove { .. } | Input::Resize { .. } => return Ok(()),
        }
        self.input_done.fetch_max(event.seq, Ordering::AcqRel);
        self.wake.notify_one();
        Ok(())
    }
}

/// A declared surface of a third party MCP server.
pub struct McpSurface {
    shared: Arc<Shared>,
    poll: Option<JoinHandle<()>>,
}

impl Drop for McpSurface {
    fn drop(&mut self) {
        if let Some(poll) = &self.poll {
            poll.abort();
        }
    }
}

impl McpSurface {
    /// `info` as the server declared it; `server` is its name in `manager`.
    pub fn new(manager: Manager, server: String, info: SurfaceInfo) -> Self {
        let pixel = info.mode == SurfaceMode::Pixel;
        let shared = Arc::new(Shared {
            manager,
            server,
            info: Mutex::new(info),
            feed: FrameFeed::new(4),
            input_done: AtomicU64::new(0),
            wake: Notify::new(),
            press: Mutex::new(None),
        });
        let poll = pixel.then(|| tokio::spawn(poll(Arc::downgrade(&shared))));
        Self { shared, poll }
    }

    /// The surface's info, or a mode error unless it is in `mode`.
    fn in_mode(&self, mode: SurfaceMode) -> loa_contracts::surface::Result<SurfaceInfo> {
        let info = self.info();
        if info.mode != mode {
            return Err(SurfaceError::Mode { uri: info.uri, mode: info.mode });
        }
        Ok(info)
    }
}

/// Publishes screenshots while someone watches: right away when woken, else
/// every [`POLL`].
async fn poll(shared: std::sync::Weak<Shared>) {
    loop {
        let Some(strong) = shared.upgrade() else { return };
        if strong.feed.viewers() > 0
            && let Err(error) = strong.publish().await
        {
            tracing::debug!(%error, server = strong.server, "mcp surface screenshot");
        }
        let woken = async { strong.wake.notified().await };
        let _ = tokio::time::timeout(POLL, woken).await;
    }
}

#[async_trait]
impl Surface for McpSurface {
    fn info(&self) -> SurfaceInfo {
        self.shared.info.locked().clone()
    }

    fn frames(&self) -> loa_contracts::surface::Result<broadcast::Receiver<Arc<Frame>>> {
        self.in_mode(SurfaceMode::Pixel)?;
        let receiver = self.shared.feed.subscribe();
        self.shared.wake.notify_one();
        Ok(receiver)
    }

    async fn request_keyframe(&self) -> loa_contracts::surface::Result<()> {
        self.shared.wake.notify_one();
        Ok(())
    }

    async fn input(&self, event: InputEvent) -> loa_contracts::surface::Result<()> {
        self.in_mode(SurfaceMode::Pixel)?;
        self.shared.input(&event).await
    }

    async fn document(&self) -> loa_contracts::surface::Result<Document> {
        use base64::Engine;
        let info = self.in_mode(SurfaceMode::Document)?;
        let read = self.shared.manager.read_resource(&self.shared.server, info.uri.as_str()).await;
        let contents = read.map_err(|e| SurfaceError::Backend(e.to_string()))?;
        let (mime, data) = contents
            .into_iter()
            .find_map(|c| match c {
                ResourceContents::BlobResourceContents { blob, mime_type, .. } => {
                    let data = base64::engine::general_purpose::STANDARD.decode(blob).ok()?;
                    Some((mime_type, Bytes::from(data)))
                }
                ResourceContents::TextResourceContents { text, mime_type, .. } => Some((mime_type, Bytes::from(text))),
                _ => None,
            })
            .ok_or_else(|| SurfaceError::Backend(format!("{} has no contents", info.uri)))?;
        let mime = mime.or(info.mime).unwrap_or_else(|| "application/octet-stream".to_owned());
        Ok(Document { name: info.name, mime, data })
    }

    async fn screenshot(&self) -> loa_contracts::surface::Result<Bytes> {
        self.shared.screenshot().await
    }
}
