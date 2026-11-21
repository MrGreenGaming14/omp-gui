//! Browser surface server on Chrome.
//!
//! [`BrowserServer`] is the `browser` [`SurfaceServer`](loa_contracts::surface::SurfaceServer):
//! each open tab is a pixel surface `surface://browser/<tab>`. Chrome is driven
//! over the DevTools protocol with `chromiumoxide`. Frames come from one of two
//! capture strategies (`tab::Capture`): Chrome's own `Page.startScreencast` on a
//! headless browser, or a headed browser on an Xvfb virtual display captured as
//! raw RGBA. Input always goes through CDP `Input.*` events.
//!
//! [`Tab`] also offers the structured reads the MCP layer hands to the agent:
//! [`Tab::page_info`], [`Tab::page_text`] and [`Tab::outline`].

mod chrome;
mod error;
mod input;
mod screencast;
mod server;
mod structured;
mod tab;
mod x11;
mod xvfb;

pub use chrome::{BrowserConfig, CaptureConfig, VirtualDisplayConfig, chrome_candidates, find_chrome, find_xvfb};
pub use error::{BrowserError, Result};
pub use server::BrowserServer;
pub use structured::{Bounds, OutlineNode, PageInfo};
pub use tab::Tab;
pub use x11::{Grab, X11Capture};
pub use xvfb::VirtualDisplay;

/// Frames a slow viewer may fall behind before it loses the oldest.
pub(crate) const FRAME_BACKLOG: usize = 8;

/// Server part of every browser surface uri.
pub const SERVER_NAME: &str = "browser";
