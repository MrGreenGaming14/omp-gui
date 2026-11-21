//! Session features over the journal-backed library, with a scripted harness.

use async_trait::async_trait;
use loa_bus::{Authorize, Bus, Denied, JournalOptions, NodeKey, SessionId, Store};
use loa_contracts::Summarizer;
use loa_contracts::harness::{Adapter, AdapterError, Answer, Capabilities, Feature, HarnessSession, Prompt, Result as HResult, SessionConfig, SessionState};
use loa_core::{Dom, Draft, ElementKind as K, NodeId, PatchSink, SessionMode, Value, attr};
use loa_session::hooks::{self, Advisor, TurnHooks};
use loa_session::library::Library;
use loa_session::plan::{self, Outcome, PlanGate, Verdict};
use loa_session as f;
use std::path::Path;
use std::sync::{Arc, Mutex};

struct Nobody;
impl Authorize for Nobody {
    fn authorize(&self, _: &SessionId, _: &NodeKey, _: &Draft, _: &Dom) -> Result<(), Denied> {
        Err(Denied("local only".into()))
    }
}

fn library(dir: &Path) -> Library {
    Library::new(Store::new(dir, JournalOptions::default()).unwrap(), Arc::new(Bus::new()), NodeKey([1; 32]), Arc::new(Nobody))
}

/// Records prompts and writes each as a turn with an echo reply.
struct Echo {
    sink: Arc<dyn PatchSink>,
    root: NodeId,
    adapter: Arc<Seen>,
}

#[derive(Default)]
struct Seen {
    prompts: Mutex<Vec<String>>,
    modes: Mutex<Vec<SessionMode>>,
    contexts: Mutex<Vec<Option<String>>>,
    forked: Mutex<Vec<Dom>>,
}

#[async_trait]
impl HarnessSession for Echo {
    async fn send(&self, prompt: Prompt) -> HResult<()> {
        self.adapter.prompts.lock().unwrap().push(prompt.text.clone());
        let turn = self.sink.commit(Draft::insert(self.root, K::Turn, [(attr::STATUS, Value::from("done"))], ""))?.node;
        self.sink.commit(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("user"))], prompt.text.as_str()))?;
        self.sink.commit(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("assistant"))], format!("echo {}", prompt.text)))?;
        Ok(())
    }
    async fn interrupt(&self) -> HResult<()> { Ok(()) }
    async fn answer(&self, decision: NodeId, _: Answer) -> HResult<()> { Err(AdapterError::UnknownDecision(decision)) }
    async fn set_mode(&self, mode: SessionMode) -> HResult<()> {
        self.adapter.modes.lock().unwrap().push(mode);
        Err(AdapterError::Unsupported { harness: "echo", what: "set_mode" })
    }
    fn harness(&self) -> &'static str { "echo" }
    fn state(&self) -> SessionState { SessionState::Idle }
    async fn wait_idle(&self) -> HResult<()> { Ok(()) }
    async fn shutdown(&self) -> HResult<()> { Ok(()) }
}

#[derive(Default)]
struct EchoAdapter {
    native_fork: bool,
    seen: Arc<Seen>,
}

impl EchoAdapter {
    fn session(&self, sink: Arc<dyn PatchSink>, root: NodeId) -> Box<dyn HarnessSession> {
        Box::new(Echo { sink, root, adapter: self.seen.clone() })
    }
}

#[async_trait]
impl Adapter for EchoAdapter {
    fn harness(&self) -> &'static str { "echo" }
    fn capabilities(&self) -> Capabilities {
        Capabilities { features: if self.native_fork { vec![Feature::Fork] } else { vec![] }, import: true, export: true, ..Default::default() }
    }
    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> HResult<Box<dyn HarnessSession>> {
        self.seen.contexts.lock().unwrap().push(config.context.clone());
        let attrs = [(attr::ID, Value::from(config.session_id)), (attr::HARNESS, "echo".into()), (attr::NODE, "me".into()), (attr::CREATED, Value::Int(1))];
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, K::Session, attrs, ""))?.node;
        Ok(self.session(sink, root))
    }
    async fn resume(&self, _: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> HResult<Box<dyn HarnessSession>> {
        Ok(self.session(sink, dom.root().unwrap().id))
    }
    async fn fork(&self, _: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> HResult<Box<dyn HarnessSession>> {
        self.seen.forked.lock().unwrap().push(dom.clone());
        Ok(self.session(sink, dom.root().unwrap().id))
    }
    fn import(&self, native: &Path, sink: &dyn PatchSink) -> HResult<()> {
        let text = std::fs::read_to_string(native)?;
        let attrs = [(attr::ID, Value::from("native-7")), (attr::HARNESS, "echo".into()), (attr::NODE, "".into()), (attr::CREATED, Value::Int(5))];
        let root = sink.commit_at(Draft::insert(NodeId::DOCUMENT, K::Session, attrs, ""), 5)?.node;
        for line in text.lines() {
            let turn = sink.commit_at(Draft::insert(root, K::Turn, [(attr::STATUS, Value::from("done"))], ""), 6)?.node;
            sink.commit_at(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("user"))], line), 6)?;
        }
        Ok(())
    }
    fn export(&self, dom: &Dom, out: &Path) -> HResult<()> {
        let lines: Vec<_> = dom.iter().filter(|n| n.kind == K::Message).map(|n| n.text.clone()).collect();
        Ok(std::fs::write(out, lines.join("\n"))?)
    }
}

fn config(id: &str) -> SessionConfig {
    SessionConfig { session_id: id.to_owned(), node: "me".into(), ..Default::default() }
}

fn prompt(text: &str) -> Prompt {
    Prompt { text: text.into(), by: "me".into(), ..Default::default() }
}

/// A session `s1` with two turns.
async fn two_turns(lib: &Library, adapter: &EchoAdapter) -> Vec<NodeId> {
    let live = lib.create("s1").unwrap();
    let session = adapter.start(config("s1"), live.writer.clone()).await.unwrap();
    session.send(prompt("first")).await.unwrap();
    session.send(prompt("second")).await.unwrap();
    lib.turn_seqs("s1").unwrap().into_iter().map(|(node, _)| node).collect()
}

fn root_of(lib: &Library, id: &str) -> loa_core::Node {
    lib.load(id).unwrap().root().unwrap().clone()
}

#[tokio::test]
async fn sessions_persist_fork_rewind_import_and_export() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = EchoAdapter { native_fork: true, ..Default::default() };
    let lib = library(&dir.path().join("store"));
    let turns = two_turns(&lib, &adapter).await;
    let dom = lib.load("s1").unwrap();
    assert!(lib.close("s1").unwrap());
    let lib = library(&dir.path().join("store"));
    assert_eq!(lib.load("s1").unwrap(), dom, "a closed session reloads from its journal");
    assert_eq!(lib.open("s1").unwrap().writer.with_dom(Dom::clone), dom);

    // Native fork: the journal fork labels the root, nothing more is written.
    let source = lib.through_turn("s1", turns[0]).unwrap();
    let branch = f::fork(&adapter, &lib, &source, config("b1")).await.unwrap();
    let forked = lib.load("b1").unwrap();
    let root = forked.root().unwrap();
    assert_eq!((root.str_attr(attr::ID), root.str_attr(attr::PARENT_SESSION)), (Some("b1"), Some("s1")));
    assert_eq!(root.int_attr(attr::FORKED_AT), Some(source.seq() as i64));
    assert_eq!(forked.seq(), source.seq() + 3);
    assert_eq!(adapter.seen.forked.lock().unwrap()[0], forked);
    branch.send(prompt("third")).await.unwrap();

    // Fallback rewind: a fresh session seeded with the history before the turn.
    let fallback = EchoAdapter::default();
    let target = lib.before_turn("s1", turns[1]).unwrap();
    assert!(target.get(turns[1]).is_none() && target.get(turns[0]).is_some());
    f::rewind(&fallback, &lib, &dom, &target, config("r1")).await.unwrap();
    let context = fallback.seen.contexts.lock().unwrap()[0].clone().unwrap();
    assert!(context.contains("user: first") && !context.contains("second"), "{context}");
    assert_eq!(root_of(&lib, "r1").int_attr(attr::FORKED_AT), Some(target.seq() as i64));
    assert_eq!(lib.load("s1").unwrap(), dom, "the old timeline is untouched");

    // Import, export and XML.
    let native = dir.path().join("native.txt");
    std::fs::write(&native, "alpha\nbeta").unwrap();
    lib.import(&adapter, &native, "imp-1", "my-node").unwrap();
    let root = root_of(&lib, "imp-1");
    assert_eq!((root.str_attr(attr::ID), root.str_attr(attr::NATIVE_ID), root.str_attr(attr::NODE)), (Some("imp-1"), Some("native-7"), Some("my-node")));
    let out = dir.path().join("out.txt");
    lib.export(&adapter, "imp-1", &out).unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "alpha\nbeta");
    lib.export_xml("imp-1", &out).unwrap();
    assert!(std::fs::read_to_string(&out).unwrap().contains("<session"));
    assert!(lib.import(&adapter, &dir.path().join("missing"), "imp-2", "me").is_err());
    assert!(lib.load("imp-2").is_err(), "a failed import leaves nothing behind");

    // A subagent file imports as a linked child, reused until it changes.
    let child_file = dir.path().join("child.txt");
    std::fs::write(&child_file, "sub task").unwrap();
    let live = lib.live("s1").unwrap();
    let job = live.writer.commit(Draft::insert(NodeId(1), K::Job, [(attr::KIND, Value::from("subagent"))], "")).unwrap();
    let id = lib.import_child(&adapter, "s1", job.node, &child_file, "me").unwrap();
    assert_eq!(root_of(&lib, &id).int_attr(attr::FORKED_AT), Some(job.seq as i64));
    let seq = lib.load(&id).unwrap().seq();
    assert_eq!(lib.import_child(&adapter, "s1", job.node, &child_file, "me").unwrap(), id);
    assert_eq!(lib.load(&id).unwrap().seq(), seq);
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&child_file, "sub task\nmore work").unwrap();
    lib.import_child(&adapter, "s1", job.node, &child_file, "me").unwrap();
    assert_eq!(lib.load(&id).unwrap().iter().filter(|n| n.kind == K::Turn).count(), 2);
    lib.delete("s1").unwrap();
    assert!(lib.load("s1").is_err() && lib.live("s1").is_none());
}

#[tokio::test]
async fn plan_mode_denies_tool_approvals_until_the_plan_is_approved() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = EchoAdapter::default();
    let lib = library(dir.path());
    let live = lib.create("p1").unwrap();
    let session = adapter.start(config("p1"), live.writer.clone()).await.unwrap();
    let gate = PlanGate::new();
    assert!(gate.permit("p1").is_err(), "fails closed");
    gate.enforce_through_approvals("p1");
    let permit = gate.permit("p1").unwrap();
    let step = plan::begin(&live.writer.with_dom(Dom::clone), live.writer.as_ref()).unwrap();
    assert!(gate.active("p1"));
    assert_eq!(plan::open_step(&live.writer.with_dom(Dom::clone)), Some(step));

    session.send(prompt(&plan::wrap_prompt("add a cache"))).await.unwrap();
    let turn = lib.turn_seqs("p1").unwrap()[0].0;
    let approval = live.writer.commit(Draft::insert(turn, K::Decision, [(attr::KIND, Value::from("approval")), (attr::STATUS, Value::from("waiting"))], "rm")).unwrap().node;
    let deny = gate.verdict("p1", &live.writer.with_dom(|d| d.get(approval).unwrap().clone())).unwrap();
    assert_eq!(deny.values, ["deny"]);

    let dom = live.writer.with_dom(Dom::clone);
    let text = plan::plan_text(&dom, turn).unwrap().to_owned();
    let root = dom.root().unwrap().id;
    let review = f::submit_plan(live.writer.as_ref(), root, step, &text).unwrap();
    let dom = live.writer.with_dom(Dom::clone);
    assert!(plan::is_app_plan(&dom, review) && !plan::is_app_plan(&dom, approval));
    let Outcome::Revise { permit, step, prompt: again } = plan::answer_plan(session.as_ref(), &dom, live.writer.as_ref(), review, "me", Verdict::Reject("smaller".into()), permit).await.unwrap() else { panic!("reject keeps plan mode") };
    assert!(again.contains("smaller") && again.ends_with(plan::PLAN_STEP) && gate.active("p1"));
    let review = f::submit_plan(live.writer.as_ref(), root, step, "1. small cache").unwrap();
    let dom = live.writer.with_dom(Dom::clone);
    let Outcome::Execute { prompt: go } = plan::answer_plan(session.as_ref(), &dom, live.writer.as_ref(), review, "me", Verdict::Edit("1. tiny cache".into()), permit).await.unwrap() else { panic!("edit approves") };
    assert!(go.contains("1. tiny cache") && !gate.active("p1"));
    let dom = live.writer.with_dom(Dom::clone);
    assert_eq!(dom.root().unwrap().str_attr(attr::MODE), Some("normal"));
    assert_eq!(dom.get(review).unwrap().attr(attr::ANSWER), Some(&Value::List(vec![Value::from("edit")])));
    assert_eq!(adapter.seen.modes.lock().unwrap().as_slice(), [SessionMode::Normal]);
}

/// Answers from a script and records the prompts.
#[derive(Default)]
struct Scripted {
    answers: Mutex<Vec<&'static str>>,
    asked: Mutex<Vec<String>>,
}

#[async_trait]
impl Summarizer for Scripted {
    async fn summarize(&self, _: &str, prompt: &str) -> Result<String, String> {
        self.asked.lock().unwrap().push(prompt.to_owned());
        Ok(self.answers.lock().unwrap().remove(0).to_owned())
    }
}

#[tokio::test]
async fn compaction_and_the_advisor_use_the_small_model() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = EchoAdapter::default();
    let lib = library(&dir.path().join("store"));
    let turns = two_turns(&lib, &adapter).await;
    let live = lib.live("s1").unwrap();
    let running = adapter.resume(config("s1"), &live.writer.with_dom(Dom::clone), live.writer.clone()).await.unwrap();
    let model = Arc::new(Scripted { answers: Mutex::new(vec!["They said first and second.", "You ignored the rule.", "OK."]), ..Default::default() });
    let source = live.writer.with_dom(Dom::clone);
    let request = f::Compaction { running: running.as_ref(), source: &source, source_sink: live.writer.as_ref(), next: config("s1-c"), smol_model: "smol", instructions: Some("keep it short") };
    f::compact(&adapter, &lib, model.as_ref(), request).await.unwrap().unwrap();
    let asked = model.asked.lock().unwrap()[0].clone();
    assert!(asked.contains("user: first") && asked.contains("keep it short"));
    assert!(adapter.seen.contexts.lock().unwrap()[1].as_deref().unwrap().contains("They said first and second."));
    let dom = lib.load("s1").unwrap();
    let compaction = dom.iter().find(|n| n.kind == K::Compaction).unwrap();
    assert_eq!(compaction.attr(attr::COVERS), Some(&Value::List(turns.iter().map(|t| Value::Int(t.0 as i64)).collect())));
    assert_eq!(root_of(&lib, "s1-c").str_attr(attr::PARENT_SESSION), Some("s1"));
    let history = f::translated_history(&dom).unwrap();
    assert!(history.starts_with("Summary of earlier turns:\nThey said") && !history.contains("user: first"), "{history}");

    let advisor = Advisor::new(model.clone());
    let note = advisor.review(&dom, turns[0], Some("Use tabs.")).await.unwrap().unwrap();
    assert!(model.asked.lock().unwrap()[1].contains("Use tabs.") && model.asked.lock().unwrap()[1].contains("user: first"));
    let message = hooks::record_note(live.writer.as_ref(), turns[0], &note).unwrap();
    let dom = live.writer.with_dom(Dom::clone);
    assert_eq!(dom.get(message).unwrap().str_attr(attr::BY), Some(hooks::ADVISOR));
    assert!(f::turn_transcript(&dom, turns[0]).contains("advisor: You ignored"));
    assert_eq!(advisor.review(&dom, turns[0], None).await.unwrap(), None, "OK means no note");
}

#[test]
fn rules_feed_the_context_and_each_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (home, cwd) = (dir.path().join("home"), dir.path().join("project"));
    let files = [
        (home.join(".omp/agent/rules/style.md"), "Use tabs."),
        (cwd.join(".omp/rules/style.md"), "---\nalwaysApply: true\n---\nUse four spaces."),
        (cwd.join(".omp/rules/db.md"), "---\ndescription: \"Database migrations\"\nglobs: migrations/**\n---\nNever drop columns."),
    ];
    for (path, text) in &files {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let rules = hooks::discover_rules(&cwd, &home);
    assert_eq!(rules.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["db", "style"]);
    let text = hooks::rules_context(&rules).unwrap();
    assert!(text.contains("Use four spaces.") && !text.contains("Use tabs."), "project overrides global: {text}");
    assert!(text.contains("db: Database migrations") && !text.contains("Never drop"), "{text}");
    let mut config = config("h1");
    let mut turn_hooks = TurnHooks::start(&mut config, Some(text.clone()));
    assert!(config.context.as_deref().unwrap().contains("Use four spaces."));
    assert_eq!(turn_hooks.before_turn(Some(text), "go"), "go");
    turn_hooks.advise("Mind the rule.".into());
    let sent = turn_hooks.before_turn(Some("Use tabs only.".into()), "next");
    assert!(sent.starts_with("[The project rules changed]\nUse tabs only.") && sent.contains("[Advisor note on your last turn]\nMind the rule.") && sent.ends_with("next"), "{sent}");
    assert_eq!(turn_hooks.before_turn(Some("Use tabs only.".into()), "again"), "again");
}
