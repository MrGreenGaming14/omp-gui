//! Session DOM contract shared by every crate in the app.
//!
//! - [`schema`]: element schema v1 (kinds, attributes, allowed parents)
//! - [`Dom`], [`Node`], [`NodeId`]: the typed tree with stable, never reused ids
//! - [`Patch`], [`Op`], [`Draft`]: insert, set, append and remove
//! - [`PatchSink`]: the single writer of a session

pub mod dom;
pub mod home;
pub mod patch;
pub mod schema;
pub mod sink;
pub mod value;

pub use home::home_dir;
pub use dom::{ApplyError, Descendants, Dom, Node, NodeId, PartsError};
pub use patch::{Attrs, Draft, Op, Patch};
pub use schema::{
    CallStatus, DecisionKind, DecisionStatus, ElementKind, JobKind, JobStatus, MessageKind, MessageStatus, Role,
    SCHEMA_VERSION, SchemaError, SessionMode, SurfaceMode, SurfaceStatus, TodoState, TurnStatus, answer, attr,
};
pub use sink::{CommitError, MemorySink, PatchSink, now_ms};
pub use value::{Value, ValueType};
