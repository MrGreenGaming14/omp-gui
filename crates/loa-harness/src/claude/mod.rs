//! Claude Code adapter.
//!
//! - [`ClaudeAdapter`]: drives the `claude` CLI headlessly over its stream
//!   JSON input and output mode, with the SDK control protocol for
//!   permission prompts, interrupts, model and mode changes and host tools
//! - [`events::Mapper`]: the pure state machine turning Claude Code's stream
//!   JSON messages into DOM patches
//! - [`native`]: import and export of `~/.claude/projects/<slug>/<uuid>.jsonl`

mod driver;
pub mod events;
mod mcp;
pub mod native;

pub use driver::{ClaudeAdapter, ClaudeSession, Launch, command_args, fork_point};
pub use mcp::HOST_SERVER;

/// Id written to `<session harness>`.
pub const HARNESS: &str = "claude-code";
