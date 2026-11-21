//! Client for `omp --mode rpc`.
//!
//! [`RpcClient::spawn`] starts omp, waits for its `ready` frame and
//! negotiates the protocol version ([`RpcClient::protocol_version`]). Commands
//! are typed ([`Command`]) and correlated with their responses by id.
//! Everything else omp writes arrives in order on [`Events`]: session events
//! ([`Event`], with [`Event::Unknown`] keeping frames this client doesn't
//! model), UI requests ([`UiRequest`], answered with
//! [`RpcClient::respond_ui`]) and the final [`Incoming::Exited`]. Host tool and
//! host URI callbacks are served by a [`HostHandler`].
//!
//! This module knows nothing about the session DOM, so the models, updater
//! and projects crates can drive omp through it too.

mod client;
mod command;
mod event;
mod frames;
mod inbound;

pub use crate::kit::rpc::{Result, RpcError};
pub use client::{Events, HostHandler, Incoming, MAX_PROTOCOL, MIN_PROTOCOL, Mode, RpcClient, SpawnOptions, UiResponder, omp_version};
pub use command::*;
pub use event::{
    AskOption, AskQuestion, AssistantEvent, Event, Frame, HostToolCall, HostUriRequest, PromptError, PromptStatus,
    Response, SubagentLifecycle, SubagentProgress, SubagentStatus, UiRequest, UriOperation,
};
pub use frames::{Chunk, ChunkError, OmpFrames, Reassembler};
pub use inbound::{AskAnswer, HostToolOutput, HostUriOutput, TextContent, ToolContent, UiResponse};
