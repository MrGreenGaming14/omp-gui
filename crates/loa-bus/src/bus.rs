//! Registry of live sessions and the firehose across them.

use loa_util::LockExt;
use crate::queue::{Firehose, Policy, Queue};
use crate::writer::{Hub, SessionWriter, WriterConfig};
use crate::SessionId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BusError {
    #[error("session {0} is already open")]
    AlreadyOpen(SessionId),
}

pub type Result<T, E = BusError> = std::result::Result<T, E>;

/// The live sessions of this node. Every writer opened here also feeds the
/// bus's firehose.
#[derive(Default)]
pub struct Bus {
    sessions: Mutex<HashMap<SessionId, Arc<SessionWriter>>>,
    hub: Arc<Hub>,
}

impl Bus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts the writer of a session.
    pub fn open(&self, config: WriterConfig) -> Result<Arc<SessionWriter>> {
        let mut sessions = self.sessions.locked();
        if sessions.contains_key(&config.session) {
            return Err(BusError::AlreadyOpen(config.session));
        }
        let id = config.session.clone();
        let writer = Arc::new(SessionWriter::with_hub(config, Arc::clone(&self.hub)));
        sessions.insert(id, Arc::clone(&writer));
        Ok(writer)
    }

    pub fn get(&self, session: &str) -> Option<Arc<SessionWriter>> {
        self.sessions.locked().get(session).cloned()
    }

    /// Ids of the open sessions, in no particular order.
    pub fn ids(&self) -> Vec<SessionId> {
        self.sessions.locked().keys().cloned().collect()
    }

    /// Closes a session: its writer refuses further commits and its
    /// subscriptions end. Returns the writer, or `None` if it wasn't open.
    pub fn close(&self, session: &str) -> Option<Arc<SessionWriter>> {
        let writer = self.sessions.locked().remove(session)?;
        writer.close();
        Some(writer)
    }

    /// Patches of every session on this bus from now on, tagged with their
    /// session. To start without a gap, subscribe first, then read each
    /// session's [`SessionWriter::seq`] and skip events at or below it.
    pub fn firehose(&self, policy: Policy) -> Firehose {
        let queue = Queue::new(policy);
        self.hub.subs.locked().push(Arc::clone(&queue));
        Firehose::new(queue)
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        for queue in self.hub.subs.locked().drain(..) {
            queue.close();
        }
    }
}
