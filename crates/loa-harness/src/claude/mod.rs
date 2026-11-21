//! Claude Code stream JSON adapter and native transcript codec.
pub mod account;
mod driver;
pub mod mapper;
mod mcp;
pub mod native;
pub use driver::ClaudeAdapter;
pub use mcp::HOST_SERVER;
use loa_core::attr;

/// Id written to `<session harness>`.
pub const HARNESS: &str = "claude-code";

/// Usage attributes by their field in an API `usage` object.
const USAGE: [(&str, &str); 4] = [
    (attr::INPUT_TOKENS, "input_tokens"),
    (attr::OUTPUT_TOKENS, "output_tokens"),
    (attr::CACHE_READ_TOKENS, "cache_read_input_tokens"),
    (attr::CACHE_WRITE_TOKENS, "cache_creation_input_tokens"),
];
