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
//! - [`cli::run`] is the stdio server's command line (the binary and the
//!   app's `omp-bridge` subcommand); [`session::session_spec`] is the MCP
//!   server spec a session of another harness gets.
//! - [`list_omp_tools`] reports omp's own tools for the Tools panel.

pub mod cli;
mod error;
mod omp;
mod provider;
mod rpc;
mod server;
pub mod session;
mod tool;

pub use error::{Error, Result};
pub use omp::{DEFAULT_TOOLS, OmpBridgeConfig, OmpExecutor, list_omp_tools};
pub use server::{BridgeServer, serve, serve_stdio};
pub use tool::{OutputBlock, ToolExecutor, ToolOutput, ToolSpec};
