//! omp tools served to other harnesses as an MCP server.
//!
//! omp has no command that runs one of its tools directly (see
//! `docs/omp-tools-bridge.md`). The bridge therefore runs `omp --mode rpc`
//! against a loopback model provider that it serves itself: every MCP
//! `tools/call` becomes an RPC `prompt`, the scripted provider answers that
//! prompt with exactly the requested tool call, omp executes its real tool, and
//! the bridge returns the `tool_execution_end` result to the MCP client. No
//! model is ever contacted.
//!
//! - [`ToolExecutor`] is the port the MCP server runs tools through.
//! - [`OmpExecutor`] implements it on top of an omp child process.
//! - [`BridgeServer`], [`serve`] and [`serve_stdio`] expose any executor over MCP.

mod error;
mod omp;
mod provider;
mod rpc;
mod server;
mod tool;

pub use error::{Error, Result};
pub use omp::{DEFAULT_TOOLS, OmpBridgeConfig, OmpExecutor};
pub use server::{BridgeServer, serve, serve_stdio};
pub use tool::{OutputBlock, ToolExecutor, ToolOutput, ToolSpec};
