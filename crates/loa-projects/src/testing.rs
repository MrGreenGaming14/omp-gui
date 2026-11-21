//! The crate's one set of test fixtures: DOM builder, in-memory sessions, fake summarizer, live sessions.

use crate::digest::{DigestContent, write_drafts};
use crate::digester::{Summarizer, Summarizers};
use crate::rebuild::SessionWriter;
use crate::source::{Access, SessionInfo, SessionSource};
use crate::Result;
use async_trait::async_trait;
use loa_bus::{Bus, JournalPersist, JournalWriter, NodeKey, SessionId, Store, SyncPolicy, WriterConfig};
use loa_core::{Dom, Draft, ElementKind as K, MemorySink, NodeId, Patch, PatchSink, Value, attr};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// A session DOM under construction.
pub struct Builder {
    pub sink: Arc<MemorySink>,
    pub root: NodeId,
}

impl Builder {
    pub fn new(id: &str, project: &str, created: i64) -> Self {
        let sink = Arc::new(MemorySink::new());
        let attrs = [(attr::ID, Value::from(id)), (attr::HARNESS, "omp".into()), (attr::NODE, "node-a".into()), (attr::CREATED, created.into()), (attr::PROJECT, project.into())];
        let root = sink.commit_at(Draft::insert(NodeId::DOCUMENT, K::Session, attrs, ""), created).unwrap().node;
        Self { sink, root }
    }

    pub fn commit(&self, draft: Draft) -> NodeId {
        self.sink.commit(draft).unwrap().node
    }

    /// A finished turn with a user prompt and an assistant reply.
    pub fn turn(&self, user: &str, assistant: &str, ended: i64) -> NodeId {
        let turn = self.commit(Draft::insert(self.root, K::Turn, [(attr::STATUS, Value::from("done")), (attr::ENDED, ended.into())], ""));
        self.commit(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("user"))], user));
        self.commit(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("assistant"))], assistant));
        turn
    }

    pub fn call(&self, turn: NodeId, tool: &str, input: &str, result: &str) -> NodeId {
        let call = self.commit(Draft::insert(turn, K::Call, [(attr::TOOL, Value::from(tool))], ""));
        self.commit(Draft::insert(call, K::Input, [] as [(&str, Value); 0], input));
        self.commit(Draft::insert(call, K::Result, [] as [(&str, Value); 0], result));
        call
    }

    /// Writes a digest with `summary`, built from the current DOM.
    pub fn digest(&self, summary: &str) {
        let dom = self.dom();
        let content = DigestContent { summary: summary.into(), ..Default::default() };
        write_drafts(&dom, &content, dom.seq(), None, 0).into_iter().for_each(|d| drop(self.commit(d)));
    }

    pub fn dom(&self) -> Dom {
        self.sink.dom()
    }
}

/// Sessions in memory, readable and writable.
#[derive(Default)]
pub struct MemorySource(Mutex<BTreeMap<String, Arc<MemorySink>>>);

impl MemorySource {
    pub fn with<'a>(builders: impl IntoIterator<Item = &'a Builder>) -> Self {
        let source = Self::default();
        for b in builders {
            let id = SessionInfo::from_dom(&b.dom()).unwrap().id;
            source.0.lock().unwrap().insert(id, b.sink.clone());
        }
        source
    }
}

#[async_trait]
impl SessionSource for MemorySource {
    async fn list(&self, project: &str) -> Result<Vec<SessionInfo>> {
        let infos = self.0.lock().unwrap().values().filter_map(|s| SessionInfo::from_dom(&s.dom())).collect::<Vec<_>>();
        Ok(infos.into_iter().filter(|i| i.project == project).collect())
    }

    async fn load(&self, session: &str) -> Result<Option<Arc<Dom>>> {
        Ok(self.0.lock().unwrap().get(session).map(|s| Arc::new(s.dom())))
    }
}

impl SessionWriter for MemorySource {
    fn commit(&self, session: &str, draft: Draft) -> Result<Patch> {
        let sink = self.0.lock().unwrap().get(session).cloned().unwrap();
        Ok(sink.commit(draft)?)
    }
}

/// Shows only the listed sessions.
pub struct Only(pub &'static [&'static str]);

impl Access for Only {
    fn can_open(&self, session: &SessionInfo) -> bool {
        self.0.contains(&session.id.as_str())
    }
}

/// A summarizer that answers `digest <n>` citing T1, or fails; it records the models asked for.
#[derive(Default)]
pub struct Fake {
    pub calls: AtomicUsize,
    pub fail: bool,
    pub models: Mutex<Vec<Option<String>>>,
}

#[async_trait]
impl Summarizer for Fake {
    async fn summarize(&self, _: &str, _: &str) -> Result<String, String> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail {
            return Err("model unavailable".into());
        }
        Ok(format!(r#"{{"summary":"digest {n}","decisions":[],"files":[],"open_items":[],"pointers":["T1"]}}"#))
    }
}

impl Summarizers for Arc<Fake> {
    fn on(&self, model: Option<&str>) -> (Arc<dyn Summarizer>, Option<String>) {
        self.models.lock().unwrap().push(model.map(str::to_owned));
        (self.clone(), Some("smol".into()))
    }
}

/// A live session on `bus`, journaled in `store`, with a `<session>` root in `cwd`.
pub fn live(bus: &Bus, store: &Store, id: &str, cwd: &str) -> Arc<loa_bus::SessionWriter> {
    let journal = JournalWriter::new(store.create(id).unwrap(), SyncPolicy::default());
    let key = NodeKey([1; 32]);
    let persist = Box::new(JournalPersist(Arc::new(journal)));
    let config = WriterConfig { session: SessionId::new(id), owner: key, local: key, dom: Dom::new(), persist, authorize: Arc::new(loa_core::testkit::AllowAll) };
    let writer = bus.open(config).unwrap();
    let attrs = [(attr::ID, Value::from(id)), (attr::HARNESS, "omp".into()), (attr::NODE, "n".into()), (attr::CREATED, 1.into()), (attr::CWD, cwd.into())];
    writer.commit_at(Draft::insert(NodeId::DOCUMENT, K::Session, attrs, ""), 1).unwrap();
    writer
}
