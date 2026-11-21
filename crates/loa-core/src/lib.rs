//! Session DOM contract shared by every crate in the app.
//!
//! - [`schema`]: element schema v1 (kinds, attributes, allowed parents)
//! - [`Dom`], [`Node`], [`NodeId`]: the typed tree with stable, never reused ids
//! - [`Patch`], [`Op`], [`Draft`]: insert, set, append and remove
//! - [`PatchSink`]: the single writer of a session; [`DomWriter`] drafts for it
//! - [`xml`]: debug export; [`rewind`]: diffs and rewind plans

pub mod dom;
pub mod patch;
pub mod rewind;
pub mod schema;
pub mod sink;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
pub mod value;
pub mod writer;
pub mod xml;

#[cfg(test)]
mod tests;

pub use dom::{ApplyError, Descendants, Dom, Node, NodeId, PartsError};
pub use patch::{Attrs, Draft, Op, Patch};
pub use schema::{
    CallStatus, DecisionKind, DecisionStatus, ElementKind, JobKind, JobStatus, MessageKind, MessageStatus, Role,
    SCHEMA_VERSION, SchemaError, SessionMode, SurfaceMode, SurfaceStatus, TodoState, TurnStatus, answer, attr,
};
pub use sink::{CommitError, MemorySink, PatchSink, now_ms};
pub use value::{IntoAttrs, Value, ValueType};
pub use writer::DomWriter;
