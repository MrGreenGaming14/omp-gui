//! Registry of live sessions.

use loa_util::LockExt;
use crate::writer::{SessionWriter, WriterConfig};
use crate::SessionId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BusError {
    #[error("session {0} is already open")]
    AlreadyOpen(SessionId),
}

pub type Result<T, E = BusError> = std::result::Result<T, E>;

/// The live sessions of this node.
#[derive(Default)]
pub struct Bus {
    sessions: Mutex<HashMap<SessionId, Arc<SessionWriter>>>,
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
        let writer = Arc::new(SessionWriter::new(config));
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
}
