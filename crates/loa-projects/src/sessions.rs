//! The app's sessions as a [`SessionSource`] and [`SessionWriter`]: live
//! sessions through their writer on the [`Bus`], the rest from their journal
//! in the [`Store`].
//!
//! A stored session is opened only for the moment it is read or written, so
//! the app can open it right after. Loaded DOMs are cached by journal
//! sequence, so listing a project doesn't replay unchanged journals.

use crate::rebuild::SessionWriter;
use crate::source::{SessionInfo, SessionSource};
use crate::{Error, Result};
use async_trait::async_trait;
use loa_bus::Bus;
use loa_core::{Dom, Draft, Patch, PatchSink, now_ms};
use loa_journal::{Journal, Listing, Store};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

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
    fn cache(&self) -> MutexGuard<'_, HashMap<String, (u64, Arc<Dom>)>> {
        // Inserting or cloning an entry can't leave the map half written.
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn live(&self, session: &str) -> Option<Arc<Dom>> {
        self.bus.get(session).map(|writer| Arc::new(writer.with_dom(Dom::clone)))
    }

    /// Opens a stored session's journal. `Ok(None)` when it doesn't exist.
    /// A journal locked by someone else is reported as [`Error::Port`].
    fn open(&self, session: &str) -> Result<Option<Journal>> {
        match self.store.open(session) {
            Ok((journal, _)) => Ok(Some(journal)),
            Err(loa_journal::Error::NotFound(_) | loa_journal::Error::InvalidSessionId(_)) => Ok(None),
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
                return self.cache().get(session).map(|(_, dom)| Some(dom.clone())).ok_or(error);
            }
        };
        let seq = journal.last_seq();
        if let Some((cached, dom)) = self.cache().get(session)
            && *cached == seq
        {
            return Ok(Some(dom.clone()));
        }
        let dom = Arc::new(journal.load().map_err(port)?);
        drop(journal);
        self.cache().insert(session.to_owned(), (seq, dom.clone()));
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
        self.cache().insert(session.to_owned(), (seq, Arc::new(dom)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::{DigestContent, digest_of, write_drafts};
    use crate::settings::{exclude_draft, is_excluded};
    use loa_bus::{Authorize, Denied, NodeKey, Persist, PersistError, SessionId, WriterConfig};
    use loa_core::{ElementKind as K, NodeId, Value, attr};
    use loa_journal::JournalOptions;

    struct JournalPersist(Journal);

    impl Persist for JournalPersist {
        fn append(&mut self, patch: &Patch) -> Result<(), PersistError> {
            self.0.append(patch)?;
            Ok(self.0.sync()?)
        }
    }

    struct AllowAll;

    impl Authorize for AllowAll {
        fn authorize(&self, _: &SessionId, _: &NodeKey, _: &Draft, _: &Dom) -> Result<(), Denied> {
            Ok(())
        }
    }

    fn root(id: &str, cwd: &str, project: Option<&str>) -> Draft {
        let mut attrs = vec![
            (attr::ID, Value::from(id)),
            (attr::HARNESS, "omp".into()),
            (attr::NODE, "node-a".into()),
            (attr::CREATED, 1.into()),
            (attr::CWD, cwd.into()),
        ];
        if let Some(project) = project {
            attrs.push((attr::PROJECT, project.into()));
        }
        Draft::insert(NodeId::DOCUMENT, K::Session, attrs, "")
    }

    /// A stored, closed session with one finished turn.
    fn stored(store: &Store, id: &str, cwd: &str, project: Option<&str>) {
        let mut journal = store.create(id).unwrap();
        let mut dom = Dom::new();
        let mut put = |draft: Draft, dom: &mut Dom| {
            let patch = dom.commit(draft, 1).unwrap();
            journal.append_applied(&patch, dom).unwrap();
            patch.node
        };
        let root = put(root(id, cwd, project), &mut dom);
        let turn = put(Draft::insert(root, K::Turn, [(attr::STATUS, Value::from("done"))], ""), &mut dom);
        put(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("user"))], "hello from the journal"), &mut dom);
        journal.sync().unwrap();
    }

    /// A live session on the bus, persisted to a new journal.
    fn live(bus: &Bus, store: &Store, id: &str, project: &str) -> Arc<loa_bus::SessionWriter> {
        let journal = store.create(id).unwrap();
        let writer = bus
            .open(WriterConfig {
                session: SessionId::new(id),
                owner: NodeKey([1; 32]),
                local: NodeKey([1; 32]),
                dom: Dom::new(),
                persist: Box::new(JournalPersist(journal)),
                authorize: Arc::new(AllowAll),
            })
            .unwrap();
        writer.commit_at(root(id, "/work/x", Some(project)), 2).unwrap();
        writer
    }

    #[tokio::test]
    async fn lists_live_and_stored_sessions_by_project() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path(), JournalOptions::default()).unwrap();
        let bus = Arc::new(Bus::new());
        stored(&store, "s1", "/home/u/loa", Some("loa"));
        stored(&store, "s2", "/home/u/loa", None);
        stored(&store, "s3", "/home/u/web", None);
        let _writer = live(&bus, &store, "live1", "loa");
        let sessions = LiveSessions::new(bus, store);
        let mut ids: Vec<String> = sessions.list("loa").await.unwrap().into_iter().map(|i| i.id).collect();
        ids.sort();
        // A session without a PROJECT attr belongs to no project, whatever its cwd.
        assert_eq!(ids, ["live1", "s1"]);
        assert!(sessions.list("web").await.unwrap().is_empty());
        let dom = sessions.load("s1").await.unwrap().unwrap();
        assert!(dom.iter().any(|n| n.text == "hello from the journal"));
        assert!(sessions.load("missing").await.unwrap().is_none());
        assert!(sessions.load("../etc").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn writes_reach_live_sessions_and_closed_journals() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path(), JournalOptions::default()).unwrap();
        let bus = Arc::new(Bus::new());
        stored(&store, "closed", "/home/u/loa", None);
        let writer = live(&bus, &store, "open", "loa");
        let sessions = LiveSessions::new(bus, store.clone());

        // Exclude toggle on a closed session is persisted to its journal.
        let dom = sessions.load("closed").await.unwrap().unwrap();
        SessionWriter::commit(&sessions, "closed", exclude_draft(&dom, true).unwrap()).unwrap();
        assert!(is_excluded(&sessions.load("closed").await.unwrap().unwrap()));
        let (journal, _) = store.open("closed").unwrap();
        assert!(is_excluded(&journal.load().unwrap()));
        drop(journal);

        // A digest written to a live session goes through its writer.
        let content = DigestContent { summary: "live digest".into(), ..Default::default() };
        let dom = sessions.load("open").await.unwrap().unwrap();
        for draft in write_drafts(&dom, &content, dom.seq(), None, 3) {
            sessions.commit("open", draft).await.unwrap();
        }
        assert_eq!(writer.with_dom(|d| digest_of(d).unwrap().content.summary), "live digest");
    }
}
