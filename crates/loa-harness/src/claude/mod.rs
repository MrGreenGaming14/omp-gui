//! Claude Code stream JSON adapter and native transcript codec.
mod driver;
pub mod mapper;
mod mcp;
pub mod native;
mod normalize;
mod protocol;
pub use driver::ClaudeAdapter;
pub use mcp::HOST_SERVER;
/// Id written to `<session harness>`.
pub const HARNESS: &str = "claude-code";
