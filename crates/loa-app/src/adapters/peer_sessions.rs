//! This node's sessions as peers see them, read from the app's [`Library`].

use loa_bus::{Listing, SessionWriter};
use loa_p2p::{LocalSession, LocalSessions};
use loa_session::library::Library;
use loa_util::LockExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Every session in the app's [`Library`], live or only journaled. Peers'
/// requests open a session's writer through the library, so its changes are
/// journaled like the user's own.
pub struct LibrarySessions {
    library: Arc<Library>,
    /// Sessions read from their journals, by id, with the sequence they were read at.
    cache: Mutex<HashMap<String, LocalSession>>,
}

impl LibrarySessions {
    pub fn new(library: Arc<Library>) -> Self {
        Self { library, cache: Mutex::default() }
    }

    /// `id` at journal sequence `seq`, read again only when the journal moved.
    fn closed(&self, id: &str, seq: u64) -> Option<LocalSession> {
        let mut cache = self.cache.locked();
        if let Some(session) = cache.get(id).filter(|s| s.last_seq == seq) {
            return Some(session.clone());
        }
        let session = LocalSession::from_dom(&self.library.load(id).ok()?)?;
        cache.insert(id.to_owned(), session.clone());
        Some(session)
    }
}

impl LocalSessions for LibrarySessions {
    fn sessions(&self) -> Vec<LocalSession> {
        let Ok(listings) = self.library.store().list() else { return Vec::new() };
        let mut out: Vec<LocalSession> = listings
            .into_iter()
            .filter_map(|listing| match listing {
                Listing::Session(info) => Some(info),
                Listing::Unreadable { .. } => None,
            })
            .filter_map(|info| match self.library.live(&info.id) {
                Some(live) => live.writer.is_writer().then(|| live.writer.with_dom(LocalSession::from_dom)).flatten(),
                None => self.closed(&info.id, info.last_seq),
            })
            .collect();
        out.sort_by(|a, b| b.facts.created_ms.cmp(&a.facts.created_ms).then_with(|| a.facts.id.cmp(&b.facts.id)));
        out
    }

    fn writer(&self, id: &str) -> Option<Arc<SessionWriter>> {
        let live = self.library.open(id).ok()?;
        live.writer.is_writer().then_some(live.writer)
    }
}
