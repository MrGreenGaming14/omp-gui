//! OpenCode over its HTTP server (`opencode serve`) and its export files.

pub mod client;
mod driver;
pub mod event;
mod ids;
mod json;
pub mod mapper;
mod native;

pub use driver::{OpenCodeAdapter, OpenCodeSession, provider_models};
pub use mapper::HARNESS;

/// Where `opencode serve` listens by default.
pub const DEFAULT_URL: &str = "http://127.0.0.1:4096";
pub use native::{OpenCodeNative, export_value};
