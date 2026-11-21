//! The app's sessions as a [`SessionSource`] and [`SessionWriter`]: live ones through their bus
//! writer, the rest from their journal, opened only while read or written. Stored DOMs are cached
//! by journal sequence, so listing a project doesn't replay unchanged journals.

use crate::rebuild::SessionWriter;
use crate::source::{SessionInfo, SessionSource};
use crate::{Error, Result};
use async_trait::async_trait;
use loa_bus::{Bus, Journal, JournalError, Listing, Store};
use loa_core::{Dom, Draft, Patch, PatchSink, now_ms};
use loa_util::LockExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Every session of this node, live or stored.
#[derive(Clone)]
pub struct LiveSessions {
    inner: Arc<Inner>,
}

struct Inner {
    bus: Arc<Bus>,
    store: Store,
    /// Stored sessions by id with the journal sequence they were loaded at.
    cache: Mutex<HashMap<String, (u64, Arc<Dom>)>>,
}

fn port(error: impl std::fmt::Display) -> Error {
    Error::Port(error.to_string())
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(work).await.map_err(port)?
}

impl LiveSessions {
    pub fn new(bus: Arc<Bus>, store: Store) -> Self {
        Self { inner: Arc::new(Inner { bus, store, cache: Mutex::new(HashMap::new()) }) }
    }

    /// Every session that belongs to a project, live ones first, with its DOM. Unreadable ones are logged.
    pub async fn all(&self) -> Result<Vec<(SessionInfo, Arc<Dom>)>> {
        let inner = self.inner.clone();
        blocking(move || inner.all()).await
    }

    pub async fn commit(&self, session: &str, draft: Draft) -> Result<Patch> {
        let (inner, session) = (self.inner.clone(), session.to_owned());
        blocking(move || inner.commit(&session, draft)).await
    }
}

impl Inner {
    fn live(&self, session: &str) -> Option<Arc<Dom>> {
        self.bus.get(session).map(|writer| Arc::new(writer.with_dom(Dom::clone)))
    }

    /// `Ok(None)` when the journal doesn't exist; one locked elsewhere is an [`Error::Port`].
    fn open(&self, session: &str) -> Result<Option<Journal>> {
        match self.store.open(session) {
            Ok((journal, _)) => Ok(Some(journal)),
            Err(JournalError::NotFound(_) | JournalError::InvalidSessionId(_)) => Ok(None),
            Err(e) => Err(port(e)),
        }
    }

    fn load(&self, session: &str) -> Result<Option<Arc<Dom>>> {
        if let Some(dom) = self.live(session) {
            return Ok(Some(dom));
        }
        let journal = match self.open(session) {
            Ok(Some(journal)) => journal,
            Ok(None) => return Ok(None),
            // The app may have opened it since the bus check.
            Err(error) => return self.live(session).or_else(|| self.cache.locked().get(session).map(|(_, dom)| dom.clone())).map(Some).ok_or(error),
        };
        let seq = journal.last_seq();
        if let Some((cached, dom)) = self.cache.locked().get(session)
            && *cached == seq
        {
            return Ok(Some(dom.clone()));
        }
        let dom = Arc::new(journal.load().map_err(port)?);
        drop(journal);
        self.cache.locked().insert(session.to_owned(), (seq, dom.clone()));
        Ok(Some(dom))
    }

    fn all(&self) -> Result<Vec<(SessionInfo, Arc<Dom>)>> {
        let mut ids: Vec<String> = self.bus.ids().iter().map(|id| id.as_str().to_owned()).collect();
        for listing in self.store.list().map_err(port)? {
            match listing {
                Listing::Session(info) if !ids.contains(&info.id) => ids.push(info.id),
                Listing::Session(_) => {}
                Listing::Unreadable { id, error } => tracing::warn!(session = %id, %error, "skipping an unreadable session journal"),
            }
        }
        let mut out = Vec::new();
        for id in ids {
            match self.load(&id) {
                Ok(dom) => out.extend(dom.and_then(|dom| Some((SessionInfo::from_dom(&dom)?, dom)))),
                Err(error) => tracing::warn!(session = %id, %error, "skipping a session that can't be read"),
            }
        }
        Ok(out)
    }

    fn commit(&self, session: &str, draft: Draft) -> Result<Patch> {
        if let Some(writer) = self.bus.get(session) {
            return Ok(writer.commit_at(draft, now_ms())?);
        }
        let mut journal = self.open(session)?.ok_or_else(|| Error::SessionNotFound(session.to_owned()))?;
        let mut dom = journal.load().map_err(port)?;
        let patch = dom.commit(draft, now_ms()).map_err(loa_core::CommitError::from)?;
        journal.append_applied(&patch, &dom).map_err(port)?;
        journal.sync().map_err(port)?;
        let seq = journal.last_seq();
        drop(journal);
        self.cache.locked().insert(session.to_owned(), (seq, Arc::new(dom)));
        Ok(patch)
    }
}

#[async_trait]
impl SessionSource for LiveSessions {
    async fn list(&self, project: &str) -> Result<Vec<SessionInfo>> {
        Ok(self.all().await?.into_iter().map(|(info, _)| info).filter(|info| info.project == project).collect())
    }

    async fn load(&self, session: &str) -> Result<Option<Arc<Dom>>> {
        let (inner, session) = (self.inner.clone(), session.to_owned());
        blocking(move || inner.load(&session)).await
    }
}

impl SessionWriter for LiveSessions {
    fn commit(&self, session: &str, draft: Draft) -> Result<Patch> {
        self.inner.commit(session, draft)
    }
}
