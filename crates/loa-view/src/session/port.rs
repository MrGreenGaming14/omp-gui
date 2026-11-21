//! Ports the session screen needs and the app implements.

use loa_core::{DecisionKind, NodeId};
use std::pin::Pin;

/// A system notification about a session that needs input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub session: String,
    pub session_title: Option<String>,
    pub decision: NodeId,
    pub kind: DecisionKind,
    /// The notification's title, e.g. "A session needs approval".
    pub heading: String,
    /// One line about what is asked: tool and command, question, title.
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotifyError(pub String);

impl std::fmt::Display for NotifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "system notification failed: {}", self.0)
    }
}

/// The future a [`Notifier`] returns.
pub type NotifyFuture = Pin<Box<dyn Future<Output = Result<(), NotifyError>> + Send + 'static>>;

/// Raises notifications off the UI thread.
pub trait Notifier: Send + Sync {
    fn notify(&self, notice: &Notice) -> NotifyFuture;
}
