//! The app's sessions as a [`SessionSource`] and [`SessionWriter`]: live
//! sessions through their writer on the [`Bus`], the rest from their journal
//! in the [`Store`].
//!
//! A stored session is opened only for the moment it is read or written, so
//! the app can open it right after. Loaded DOMs are cached by journal
//! sequence, so listing a project doesn't replay unchanged journals.

use loa_util::LockExt;
use crate::rebuild::SessionWriter;
use crate::source::{SessionInfo, SessionSource};
use crate::{Error, Result};
use async_trait::async_trait;
use loa_bus::Bus;
use loa_core::{Dom, Draft, Patch, PatchSink, now_ms};
use loa_bus::{Journal, Listing, Store};
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
    /// Stored sessions by id: the journal sequence a DOM was loaded at.
    cache: Mutex<HashMap<String, (u64, Arc<Dom>)>>,
}

fn port(error: impl std::fmt::Display) -> Error {
    Error::Port(error.to_string())
}

impl LiveSessions {
    pub fn new(bus: Arc<Bus>, store: Store) -> Self {
        Self { inner: Arc::new(Inner { bus, store, cache: Mutex::new(HashMap::new()) }) }
    }

    /// Every session that belongs to a project, live ones first, each with
    /// its current DOM. Unreadable journals are skipped and logged.
    pub async fn all(&self) -> Result<Vec<(SessionInfo, Arc<Dom>)>> {
        let inner = self.inner.clone();
        blocking(move || inner.all()).await
    }

    /// Commits `draft` to `session` now, wherever it lives.
    pub async fn commit(&self, session: &str, draft: Draft) -> Result<Patch> {
        let inner = self.inner.clone();
        let session = session.to_owned();
        blocking(move || inner.commit(&session, draft)).await
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(work).await.map_err(port)?
}

impl Inner {
    fn live(&self, session: &str) -> Option<Arc<Dom>> {
        self.bus.get(session).map(|writer| Arc::new(writer.with_dom(Dom::clone)))
    }

    /// Opens a stored session's journal. `Ok(None)` when it doesn't exist.
    /// A journal locked by someone else is reported as [`Error::Port`].
    fn open(&self, session: &str) -> Result<Option<Journal>> {
        match self.store.open(session) {
            Ok((journal, _)) => Ok(Some(journal)),
            Err(loa_bus::JournalError::NotFound(_) | loa_bus::JournalError::InvalidSessionId(_)) => Ok(None),
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
            Err(error) => {
                // The app opened it between the bus check and now.
                if let Some(dom) = self.live(session) {
                    return Ok(Some(dom));
                }
                return self.cache.locked().get(session).map(|(_, dom)| Some(dom.clone())).ok_or(error);
            }
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
        let mut out = Vec::new();
        let mut live: Vec<String> = Vec::new();
        for id in self.bus.ids() {
            if let Some(dom) = self.live(id.as_str()) {
                live.push(id.as_str().to_owned());
                if let Some(info) = SessionInfo::from_dom(&dom) {
                    out.push((info, dom));
                }
            }
        }
        for listing in self.store.list().map_err(port)? {
            let id = match listing {
                Listing::Session(info) => info.id,
                Listing::Unreadable { id, error } => {
                    tracing::warn!(session = %id, %error, "skipping an unreadable session journal");
                    continue;
                }
            };
            if live.contains(&id) {
                continue;
            }
            match self.load(&id) {
                Ok(Some(dom)) => {
                    if let Some(info) = SessionInfo::from_dom(&dom) {
                        out.push((info, dom));
                    }
                }
                Ok(None) => {}
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
        let inner = self.inner.clone();
        let session = session.to_owned();
        blocking(move || inner.load(&session)).await
    }
}

impl SessionWriter for LiveSessions {
    fn commit(&self, session: &str, draft: Draft) -> Result<Patch> {
        self.inner.commit(session, draft)
    }
}
