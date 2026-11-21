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
pub use native::{OpenCodeNative, export_value};
