//! What the transcript asks of the app.

use loa_core::NodeId;

/// What the transcript asks the app to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    /// A user message was clicked: rewind the session to it.
    Rewind(NodeId),
    OpenLink(String),
}
