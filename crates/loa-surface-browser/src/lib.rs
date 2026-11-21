//! Browser surface server on Chrome.
//!
//! [`BrowserServer`] is the `browser` [`SurfaceServer`](loa_surface::SurfaceServer):
//! each open tab is a pixel surface `surface://browser/<tab>`. Chrome is driven
//! over the DevTools protocol with `chromiumoxide`. Frames come from one of two
//! capture strategies ([`Capture`]): Chrome's own `Page.startScreencast` on a
//! headless browser, or a headed browser on an Xvfb virtual display captured as
//! raw RGBA. Input always goes through CDP `Input.*` events.
//!
//! [`Tab`] also offers the structured reads the MCP layer hands to the agent:
//! [`Tab::page_info`], [`Tab::page_text`] and [`Tab::outline`].

pub mod bench;
mod chrome;
mod error;
mod frames;
mod input;
mod screencast;
mod server;
mod structured;
mod tab;
#[cfg(target_os = "linux")]
mod x11;
#[cfg(target_os = "linux")]
mod xvfb;

pub use chrome::{
    BrowserConfig, Capture, VIRTUAL_DISPLAY_UNAVAILABLE, VirtualDisplayConfig, chrome_candidates, find_chrome, find_xvfb,
};
pub use error::{BrowserError, Result};
pub use server::BrowserServer;
pub use structured::{Bounds, OutlineNode, PageInfo};
pub use tab::Tab;
#[cfg(target_os = "linux")]
pub use x11::{Grab, X11Capture};
#[cfg(target_os = "linux")]
pub use xvfb::VirtualDisplay;

/// Server part of every browser surface uri.
pub const SERVER_NAME: &str = "browser";
