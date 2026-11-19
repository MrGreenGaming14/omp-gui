use async_trait::async_trait;
use loa_adapter::{Adapter, AdapterError, Capabilities, Feature, HarnessSession, ModelSelection, ModelSwitch, Prompt, SessionConfig, SessionState};
use loa_core::{attr, Dom, Draft, ElementKind as K, MemorySink, NodeId, PatchSink, Value};
use loa_features::{PlanPermit, ReadOnlyTools, Result, SessionStore, Summarizer};
use std::{sync::{Arc, Mutex}, collections::HashMap};

#[derive(Default)]
struct Store {
    sinks: Mutex<HashMap<String, Arc<MemorySink>>>,
    source: Mutex<Option<Dom>>,
}

impl Store {
    fn dom(&self, id: &str) -> Dom { self.sinks.lock().unwrap()[id].dom() }
}

#[async_trait]
impl SessionStore for Store {
    async fn fork(&self, _source_id: &str, seq: u64, id: &str) -> Result<(Dom, Arc<dyn PatchSink>)> {
        let source = self.source.lock().unwrap().clone().unwrap();
        assert_eq!(seq, source.seq());
        let sink = Arc::new(MemorySink::from_dom(source.clone()));
        self.sinks.lock().unwrap().insert(id.to_owned(), sink.clone());
        Ok((source, sink))
    }
    async fn fresh(&self, id: &str) -> Result<Arc<dyn PatchSink>> {
        let sink = Arc::new(MemorySink::new());
        self.sinks.lock().unwrap().insert(id.to_owned(), sink.clone());
        Ok(sink)
    }
}

struct Harness {
    state: SessionState,
    selected: Arc<Mutex<Vec<String>>>,
    prompts: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl HarnessSession for Harness {
    async fn send(&self, prompt: Prompt) -> loa_adapter::Result<()> {
        self.prompts.lock().unwrap().push(prompt.text);
        Ok(())
    }
    async fn interrupt(&self) -> loa_adapter::Result<()> { Ok(()) }
    async fn answer(&self, _: NodeId, _: loa_adapter::Answer) -> loa_adapter::Result<()> { Ok(()) }
    async fn set_model(&self, selection: ModelSelection) -> loa_adapter::Result<()> {
        self.selected.lock().unwrap().push(selection.model);
        Ok(())
    }
    fn harness(&self) -> &'static str { "test" }
    fn state(&self) -> SessionState { self.state }
    async fn wait_idle(&self) -> loa_adapter::Result<()> { Ok(()) }
    async fn shutdown(&self) -> loa_adapter::Result<()> { Ok(()) }
}

struct TestAdapter {
    native_fork: bool,
    selected: Arc<Mutex<Vec<String>>>,
    prompts: Arc<Mutex<Vec<String>>>,
}

impl TestAdapter {
    fn new(native_fork: bool) -> Self {
        Self { native_fork, selected: Arc::default(), prompts: Arc::default() }
    }
    fn harness(&self) -> Box<dyn HarnessSession> {
        Box::new(Harness { state: SessionState::Idle, selected: self.selected.clone(), prompts: self.prompts.clone() })
    }
}

#[async_trait]
impl Adapter for TestAdapter {
    fn harness(&self) -> &'static str { "test" }
    fn capabilities(&self) -> Capabilities {
        Capabilities { features: if self.native_fork { vec![Feature::Fork] } else { vec![] }, model_switch: ModelSwitch::PerCall, ..Default::default() }
    }
    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        sink.commit(Draft::insert(NodeId::DOCUMENT, K::Session, [
            (attr::ID, Value::from(config.session_id)),
            (attr::HARNESS, Value::from("test")),
            (attr::NODE, Value::from("owner")),
            (attr::CREATED, Value::Int(1)),
        ], ""))?;
        Ok(self.harness())
    }
    async fn resume(&self, _: SessionConfig, _: &Dom, _: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        Err(AdapterError::Unsupported { harness: "test", what: "resume" })
    }
    async fn fork(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        assert_eq!(dom.root().unwrap().str_attr(attr::ID), Some(config.session_id.as_str()));
        assert_eq!(dom.seq(), sink_as_seq(&sink, dom.seq()));
        Ok(self.harness())
    }
}

fn sink_as_seq(_: &Arc<dyn PatchSink>, expected: u64) -> u64 { expected }

fn source() -> (MemorySink, Dom) {
    let sink = MemorySink::new();
    let root = sink.commit(Draft::insert(NodeId::DOCUMENT, K::Session, [
        (attr::ID, Value::from("source")), (attr::HARNESS, Value::from("test")),
        (attr::NODE, Value::from("owner")), (attr::CREATED, Value::Int(1)),
    ], "")).unwrap().node;
    let turn = sink.commit(Draft::insert(root, K::Turn, [(attr::STATUS, Value::from("done"))], "")).unwrap().node;
    sink.commit(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("user"))], "Hello")).unwrap();
    let dom = sink.dom();
    (sink, dom)
}

fn config(id: &str) -> SessionConfig { SessionConfig { session_id: id.to_owned(), ..Default::default() } }

#[tokio::test]
async fn fallback_fork_and_rewind_seed_history_and_lineage() {
    let (_, dom) = source();
    let store = Store::default();
    let adapter = TestAdapter::new(false);
    let branch = loa_features::fork(&adapter, &store, &dom, config("branch")).await.unwrap();
    assert!(branch.config.context.as_deref().unwrap().contains("user: Hello"));
    let root = store.dom("branch").root().unwrap().clone();
    assert_eq!(root.str_attr(attr::PARENT_SESSION), Some("source"));
    assert_eq!(root.int_attr(attr::FORKED_AT), Some(dom.seq() as i64));
    let (_, rewound) = loa_features::rewind(&adapter, &store, &dom, &dom, config("rewound")).await.unwrap();
    assert!(rewound.config.context.as_deref().unwrap().contains("Hello"));
}

#[tokio::test]
async fn native_fork_preserves_dom_and_relabels_root() {
    let (_, dom) = source();
    let store = Store::default();
    *store.source.lock().unwrap() = Some(dom.clone());
    let adapter = TestAdapter::new(true);
    let branch = loa_features::fork(&adapter, &store, &dom, config("native")).await.unwrap();
    assert!(branch.config.context.is_none());
    let forked = store.dom("native");
    assert_eq!(forked.root().unwrap().str_attr(attr::ID), Some("native"));
    assert_eq!(forked.children_of_kind(forked.root().unwrap().id, K::Turn).count(), 1);
    assert_eq!(forked.seq(), dom.seq() + 3);
}

struct Summary;
#[async_trait]
impl Summarizer for Summary {
    async fn summarize(&self, model: &str, history: &str, _: Option<&str>) -> Result<String> {
        assert_eq!(model, "smol");
        assert!(history.contains("Hello"));
        Ok("The user greeted the agent.".into())
    }
}

#[tokio::test]
async fn compaction_records_covered_turn_and_starts_fresh_session() {
    let (sink, dom) = source();
    let store = Store::default();
    let adapter = TestAdapter::new(false);
    let running = adapter.harness();
    let next = loa_features::compact(&adapter, &store, &Summary, loa_features::Compaction {
        running: running.as_ref(), source: &dom, source_sink: &sink,
        next: config("compact"), smol_model: "smol", instructions: None,
    }).await.unwrap().unwrap();
    assert!(next.config.context.unwrap().contains("The user greeted"));
    let updated = sink.dom();
    let compaction = updated.children_of_kind(updated.root().unwrap().id, K::Compaction).next().unwrap();
    assert_eq!(compaction.attr(attr::MODEL), Some(&Value::from("smol")));
    assert_eq!(compaction.attr(attr::COVERS), Some(&Value::List(vec![Value::Int(2)])));
    assert_eq!(updated.children_of_kind(updated.root().unwrap().id, K::Turn).count(), 1);
}

#[test]
fn translated_history_places_summary_before_later_turns() {
    let (sink, dom) = source();
    let session = dom.root().unwrap().id;
    sink.commit(Draft::insert(session, K::Compaction, [
        (attr::COVERS, Value::List(vec![Value::Int(2)])),
    ], "Earlier greeting")).unwrap();
    let turn = sink.commit(Draft::insert(session, K::Turn,
        [(attr::STATUS, Value::from("done"))], "")).unwrap().node;
    sink.commit(Draft::insert(turn, K::Message,
        [(attr::ROLE, Value::from("user"))], "Later question")).unwrap();
    let history = loa_features::translated_history(&sink.dom()).unwrap();
    assert!(!history.contains("Hello"));
    assert!(history.find("Earlier greeting").unwrap() < history.find("Later question").unwrap());
}

struct Gate;
struct Permit;
impl PlanPermit for Permit {}
#[async_trait]
impl ReadOnlyTools for Gate {
    async fn activate(&self, _: &str) -> Result<Box<dyn PlanPermit>> { Ok(Box::new(Permit)) }
}

#[tokio::test]
async fn child_job_plan_review_and_model_switch_are_visible() {
    let (sink, dom) = source();
    let adapter = TestAdapter::new(false);
    let store = Store::default();
    let (job, child) = loa_features::subagent(&adapter, &store, &dom, &sink, dom.root().unwrap().id,
        config("child"), Prompt { text: "investigate".into(), ..Default::default() }).await.unwrap();
    assert_eq!(sink.dom().get(job).unwrap().str_attr(attr::SESSION), Some("child"));
    assert_eq!(adapter.prompts.lock().unwrap().as_slice(), ["investigate"]);
    loa_features::finish_subagent(child.harness.as_ref(), &sink, job).await.unwrap();
    assert_eq!(sink.dom().get(job).unwrap().str_attr(attr::STATUS), Some("done"));
    let harness = adapter.harness();
    let (permit, todo) = loa_features::enter_plan(harness.as_ref(), &dom, &sink, &Gate).await.unwrap();
    let decision = loa_features::submit_plan(&sink, dom.root().unwrap().id, todo, "Read files first").unwrap();
    assert_eq!(sink.dom().get(decision).unwrap().text, "Read files first");
    let outcome = loa_features::plan::answer_plan(harness.as_ref(), &sink.dom(), &sink, decision, "user",
        loa_features::plan::Verdict::Approve, permit).await.unwrap();
    assert!(matches!(outcome, loa_features::plan::Outcome::Execute { prompt } if prompt.contains("Read files first")));
    assert_eq!(sink.dom().get(decision).unwrap().str_attr(attr::STATUS), Some("answered"));
    loa_features::select_model(harness.as_ref(), &adapter.capabilities(), &sink,
        dom.root().unwrap().id, "smol", "small-model").await.unwrap();
    assert_eq!(sink.dom().root().unwrap().str_attr(attr::MODEL_ROLE), Some("smol"));
    assert_eq!(adapter.selected.lock().unwrap().as_slice(), ["small-model"]);
}
