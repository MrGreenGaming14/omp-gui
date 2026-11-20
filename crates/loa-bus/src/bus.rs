//! Registry of live sessions.

use crate::writer::{SessionWriter, WriterConfig};
use crate::SessionId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

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

    fn sessions(&self) -> MutexGuard<'_, HashMap<SessionId, Arc<SessionWriter>>> {
        // Map inserts and removes are single steps; a poisoned map is intact.
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts the writer of a session.
    pub fn open(&self, config: WriterConfig) -> Result<Arc<SessionWriter>> {
        let mut sessions = self.sessions();
        if sessions.contains_key(&config.session) {
            return Err(BusError::AlreadyOpen(config.session));
        }
        let id = config.session.clone();
        let writer = Arc::new(SessionWriter::new(config));
        sessions.insert(id, Arc::clone(&writer));
        Ok(writer)
    }

    pub fn get(&self, session: &str) -> Option<Arc<SessionWriter>> {
        self.sessions().get(session).cloned()
    }

    /// Ids of the open sessions, in no particular order.
    pub fn ids(&self) -> Vec<SessionId> {
        self.sessions().keys().cloned().collect()
    }

    /// Closes a session: its writer refuses further commits and its
    /// subscriptions end. Returns the writer, or `None` if it wasn't open.
    pub fn close(&self, session: &str) -> Option<Arc<SessionWriter>> {
        let writer = self.sessions().remove(session)?;
        writer.close();
        Some(writer)
    }
}
