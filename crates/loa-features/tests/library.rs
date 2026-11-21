//! The journal-backed library and the features that go through it, with a
//! scripted harness.

use async_trait::async_trait;
use loa_contracts::harness::{Adapter, AdapterError, Answer, Capabilities, Feature, HarnessSession, Prompt, SessionConfig, SessionState};
use loa_bus::{Authorize, Bus, Denied, NodeKey, SessionId};
use loa_core::{Dom, Draft, ElementKind as K, NodeId, PatchSink, SessionMode, Value, attr};
use loa_features::library::Library;
use loa_features::plan::{self, Outcome, PlanGate, Verdict};
use loa_features::smol::OneShot;
use loa_features::hooks::{self, Advisor, TurnHooks};
use loa_bus::{JournalOptions, Store};
use std::path::Path;
use std::sync::{Arc, Mutex};

struct Nobody;
impl Authorize for Nobody {
    fn authorize(&self, _: &SessionId, _: &NodeKey, _: &Draft, _: &Dom) -> Result<(), Denied> {
        Err(Denied("local only".into()))
    }
}

fn library(dir: &Path) -> Library {
    let store = Store::new(dir, JournalOptions::default()).unwrap();
    Library::new(store, Arc::new(Bus::new()), NodeKey([1; 32]), Arc::new(Nobody))
}

/// A harness that records prompts and writes a turn with an echo reply.
struct Echo {
    sink: Arc<dyn PatchSink>,
    root: NodeId,
    prompts: Arc<Mutex<Vec<String>>>,
    modes: Arc<Mutex<Vec<SessionMode>>>,
}

#[async_trait]
impl HarnessSession for Echo {
    async fn send(&self, prompt: Prompt) -> loa_contracts::harness::Result<()> {
        self.prompts.lock().unwrap().push(prompt.text.clone());
        let turn = self.sink.commit(Draft::insert(self.root, K::Turn, [(attr::STATUS, Value::from("done"))], ""))?.node;
        self.sink.commit(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("user"))], prompt.text.as_str()))?;
        self.sink.commit(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("assistant"))], format!("echo {}", prompt.text)))?;
        Ok(())
    }
    async fn interrupt(&self) -> loa_contracts::harness::Result<()> { Ok(()) }
    async fn answer(&self, decision: NodeId, _: Answer) -> loa_contracts::harness::Result<()> { Err(AdapterError::UnknownDecision(decision)) }
    async fn set_mode(&self, mode: SessionMode) -> loa_contracts::harness::Result<()> {
        self.modes.lock().unwrap().push(mode);
        Err(AdapterError::Unsupported { harness: "echo", what: "set_mode" })
    }
    fn harness(&self) -> &'static str { "echo" }
    fn state(&self) -> SessionState { SessionState::Idle }
    async fn wait_idle(&self) -> loa_contracts::harness::Result<()> { Ok(()) }
    async fn shutdown(&self) -> loa_contracts::harness::Result<()> { Ok(()) }
}

#[derive(Default)]
struct EchoAdapter {
    native_fork: bool,
    prompts: Arc<Mutex<Vec<String>>>,
    modes: Arc<Mutex<Vec<SessionMode>>>,
    forked: Mutex<Vec<Dom>>,
}

impl EchoAdapter {
    fn session(&self, sink: Arc<dyn PatchSink>, root: NodeId) -> Box<dyn HarnessSession> {
        Box::new(Echo { sink, root, prompts: self.prompts.clone(), modes: self.modes.clone() })
    }
}

#[async_trait]
impl Adapter for EchoAdapter {
    fn harness(&self) -> &'static str { "echo" }
    fn capabilities(&self) -> Capabilities {
        Capabilities { features: if self.native_fork { vec![Feature::Fork] } else { vec![] }, import: true, export: true, ..Default::default() }
    }
    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> loa_contracts::harness::Result<Box<dyn HarnessSession>> {
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, K::Session, [
            (attr::ID, Value::from(config.session_id)), (attr::HARNESS, Value::from("echo")),
            (attr::NODE, Value::from("me")), (attr::CREATED, Value::Int(1)),
        ], ""))?.node;
        Ok(self.session(sink, root))
    }
    async fn resume(&self, _: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> loa_contracts::harness::Result<Box<dyn HarnessSession>> {
        Ok(self.session(sink, dom.root().unwrap().id))
    }
    async fn fork(&self, _: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> loa_contracts::harness::Result<Box<dyn HarnessSession>> {
        self.forked.lock().unwrap().push(dom.clone());
        Ok(self.session(sink, dom.root().unwrap().id))
    }
    fn import(&self, native: &Path, sink: &dyn PatchSink) -> loa_contracts::harness::Result<()> {
        let text = std::fs::read_to_string(native)?;
        let root = sink.commit_at(Draft::insert(NodeId::DOCUMENT, K::Session, [
            (attr::ID, Value::from("native-7")), (attr::HARNESS, Value::from("echo")),
            (attr::NODE, Value::from("")), (attr::CREATED, Value::Int(5)),
        ], ""), 5)?.node;
        for line in text.lines() {
            let turn = sink.commit_at(Draft::insert(root, K::Turn, [(attr::STATUS, Value::from("done"))], ""), 6)?.node;
            sink.commit_at(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("user"))], line), 6)?;
        }
        Ok(())
    }
    fn export(&self, dom: &Dom, out: &Path) -> loa_contracts::harness::Result<()> {
        let lines: Vec<_> = dom.iter().filter(|n| n.kind == K::Message).map(|n| n.text.clone()).collect();
        std::fs::write(out, lines.join("\n"))?;
        Ok(())
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

#[tokio::test]
async fn sessions_persist_reopen_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = EchoAdapter::default();
    let lib = library(dir.path());
    let turns = two_turns(&lib, &adapter).await;
    assert_eq!(turns.len(), 2);
    let dom = lib.load("s1").unwrap();
    assert!(lib.close("s1").unwrap());

    let reopened = library(dir.path());
    assert_eq!(reopened.load("s1").unwrap(), dom, "a closed session reloads from its journal");
    let live = reopened.open("s1").unwrap();
    assert_eq!(live.writer.with_dom(Dom::clone), dom);
    assert!(reopened.open("s1").is_ok(), "opening a live session returns it");
    reopened.delete("s1").unwrap();
    assert!(reopened.load("s1").is_err());
    assert!(reopened.live("s1").is_none());
}

#[tokio::test]
async fn native_fork_shares_the_journal_prefix_and_labels_the_root_once() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = EchoAdapter { native_fork: true, ..Default::default() };
    let lib = library(dir.path());
    let turns = two_turns(&lib, &adapter).await;
    let source = lib.through_turn("s1", turns[0]).unwrap();
    let branch = loa_features::fork(&adapter, &lib, &source, config("b1")).await.unwrap();
    let forked = lib.load("b1").unwrap();
    let root = forked.root().unwrap();
    assert_eq!(root.str_attr(attr::ID), Some("b1"));
    assert_eq!(root.str_attr(attr::PARENT_SESSION), Some("s1"));
    assert_eq!(root.int_attr(attr::FORKED_AT), Some(source.seq() as i64));
    // The journal fork labelled the root; the feature added nothing more.
    assert_eq!(forked.seq(), source.seq() + 3);
    assert_eq!(forked.children_of_kind(root.id, K::Turn).count(), 1);
    assert_eq!(adapter.forked.lock().unwrap()[0], forked);
    branch.harness.send(prompt("third")).await.unwrap();
    assert_eq!(lib.store().list().unwrap().len(), 2);
    let info = lib.store().list().unwrap().into_iter().find_map(|l| match l {
        loa_bus::Listing::Session(i) if i.id == "b1" => Some(i),
        _ => None,
    });
    assert_eq!(info.unwrap().parent.unwrap().session_id, "s1");
}

#[tokio::test]
async fn rewinding_to_a_user_message_branches_before_it() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = EchoAdapter::default();
    let lib = library(dir.path());
    let turns = two_turns(&lib, &adapter).await;
    let current = lib.load("s1").unwrap();
    let target = lib.before_turn("s1", turns[1]).unwrap();
    assert!(target.get(turns[1]).is_none());
    assert!(target.get(turns[0]).is_some());
    let (plan, rewound) = loa_features::rewind(&adapter, &lib, &current, &target, config("r1")).await.unwrap();
    assert!(plan.cancel.is_empty());
    let context = rewound.config.context.unwrap();
    assert!(context.contains("user: first") && !context.contains("second"), "{context}");
    let root = lib.load("r1").unwrap().root().unwrap().clone();
    assert_eq!(root.str_attr(attr::PARENT_SESSION), Some("s1"));
    assert_eq!(root.int_attr(attr::FORKED_AT), Some(target.seq() as i64));
    // The old timeline is untouched.
    assert_eq!(lib.load("s1").unwrap(), current);
}

#[tokio::test]
async fn import_export_and_xml_of_native_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = EchoAdapter::default();
    let lib = library(&dir.path().join("store"));
    let native = dir.path().join("native.txt");
    std::fs::write(&native, "alpha\nbeta").unwrap();
    let live = lib.import(&adapter, &native, "imp-1", "my-node").unwrap();
    let dom = live.writer.with_dom(Dom::clone);
    let root = dom.root().unwrap();
    assert_eq!(root.str_attr(attr::ID), Some("imp-1"));
    assert_eq!(root.str_attr(attr::NATIVE_ID), Some("native-7"));
    assert_eq!(root.str_attr(attr::NODE), Some("my-node"));
    assert_eq!(dom.children_of_kind(root.id, K::Turn).count(), 2);

    let out = dir.path().join("out.txt");
    lib.export(&adapter, "imp-1", &out).unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "alpha\nbeta");
    let xml = dir.path().join("out.xml");
    lib.export_xml("imp-1", &xml).unwrap();
    let xml = std::fs::read_to_string(xml).unwrap();
    assert!(xml.contains("<session") && xml.contains("alpha"), "{xml}");

    std::fs::write(&native, "").unwrap();
    let broken = dir.path().join("missing.txt");
    assert!(lib.import(&adapter, &broken, "imp-2", "my-node").is_err());
    assert!(lib.load("imp-2").is_err(), "a failed import leaves nothing behind");
}

#[tokio::test]
async fn subagent_files_import_as_linked_children_and_refresh_when_changed() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = EchoAdapter::default();
    let lib = library(&dir.path().join("store"));
    two_turns(&lib, &adapter).await;
    let live = lib.live("s1").unwrap();
    let root = live.writer.with_dom(|d| d.root().unwrap().id);
    let child_file = dir.path().join("child.txt");
    std::fs::write(&child_file, "sub task").unwrap();
    let job = live.writer.commit(Draft::insert(root, K::Job, [
        (attr::KIND, Value::from("subagent")), (attr::STATUS, Value::from("running")),
        (attr::SESSION, Value::from(child_file.to_string_lossy().as_ref())),
    ], "")).unwrap();
    let id = lib.import_child(&adapter, "s1", job.node, &child_file, "me").unwrap();
    let child = lib.load(&id).unwrap();
    let child_root = child.root().unwrap();
    assert_eq!(child_root.str_attr(attr::PARENT_SESSION), Some("s1"));
    assert_eq!(child_root.int_attr(attr::FORKED_AT), Some(job.seq as i64));
    assert_eq!(lib.import_child(&adapter, "s1", job.node, &child_file, "me").unwrap(), id, "unchanged file reuses the child");
    assert_eq!(lib.load(&id).unwrap().seq(), child.seq());

    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&child_file, "sub task\nmore work").unwrap();
    assert_eq!(lib.import_child(&adapter, "s1", job.node, &child_file, "me").unwrap(), id);
    let refreshed = lib.load(&id).unwrap();
    assert_eq!(refreshed.children_of_kind(refreshed.root().unwrap().id, K::Turn).count(), 2);
}

#[tokio::test]
async fn plan_mode_denies_tool_approvals_until_the_plan_is_approved() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = EchoAdapter::default();
    let lib = library(dir.path());
    let live = lib.create("p1").unwrap();
    let session = adapter.start(config("p1"), live.writer.clone()).await.unwrap();
    let gate = PlanGate::new();
    let dom = live.writer.with_dom(Dom::clone);
    assert!(loa_features::enter_plan(session.as_ref(), &dom, live.writer.as_ref(), &gate).await.is_err(), "fails closed");
    gate.enforce_through_approvals("p1");
    let (permit, step) = loa_features::enter_plan(session.as_ref(), &dom, live.writer.as_ref(), &gate).await.unwrap();
    assert!(gate.active("p1"));
    assert_eq!(plan::open_step(&live.writer.with_dom(Dom::clone)), Some(step));

    session.send(prompt(&plan::wrap_prompt("add a cache"))).await.unwrap();
    let dom = live.writer.with_dom(Dom::clone);
    let turn = lib.turn_seqs("p1").unwrap()[0].0;
    let approval = live.writer.commit(Draft::insert(turn, K::Decision, [
        (attr::KIND, Value::from("approval")), (attr::STATUS, Value::from("waiting")),
    ], "rm -rf target")).unwrap().node;
    let deny = gate.verdict("p1", &live.writer.with_dom(|d| d.get(approval).unwrap().clone())).unwrap();
    assert_eq!(deny.values, ["deny"]);

    let text = plan::plan_text(&dom, turn).unwrap().to_owned();
    assert!(text.contains("add a cache"));
    let root = dom.root().unwrap().id;
    let review = loa_features::submit_plan(live.writer.as_ref(), root, step, &text).unwrap();
    let dom = live.writer.with_dom(Dom::clone);
    assert!(plan::is_app_plan(&dom, review));
    assert!(!plan::is_app_plan(&dom, approval));

    let Outcome::Revise { permit, step, prompt: again } = plan::answer_plan(session.as_ref(), &dom, live.writer.as_ref(), review, "me", Verdict::Reject("smaller".into()), permit).await.unwrap() else { panic!("reject keeps plan mode") };
    assert!(again.contains("smaller") && again.ends_with(plan::PLAN_STEP));
    assert!(gate.active("p1"));
    let review = loa_features::submit_plan(live.writer.as_ref(), root, step, "1. small cache").unwrap();
    let dom = live.writer.with_dom(Dom::clone);
    let Outcome::Execute { prompt: go } = plan::answer_plan(session.as_ref(), &dom, live.writer.as_ref(), review, "me", Verdict::Edit("1. tiny cache".into()), permit).await.unwrap() else { panic!("edit approves") };
    assert!(go.contains("1. tiny cache"));
    assert!(!gate.active("p1"));
    let dom = live.writer.with_dom(Dom::clone);
    assert_eq!(dom.root().unwrap().str_attr(attr::MODE), Some("normal"));
    assert_eq!(dom.get(review).unwrap().str_attr(attr::STATUS), Some("answered"));
    assert_eq!(dom.get(review).unwrap().attr(attr::ANSWER), Some(&Value::List(vec![Value::from("edit")])));
    assert_eq!(adapter.modes.lock().unwrap().as_slice(), [SessionMode::Plan, SessionMode::Normal]);
}

/// A model that answers from a script and records the prompts.
#[derive(Default)]
struct Scripted {
    answers: Mutex<Vec<String>>,
    asked: Arc<Mutex<Vec<(String, Option<String>)>>>,
}

struct Reply(String, Option<String>, Arc<Mutex<Vec<(String, Option<String>)>>>);

#[async_trait]
impl loa_projects::Summarizer for Reply {
    async fn summarize(&self, _: &str, prompt: &str) -> Result<String, String> {
        self.2.lock().unwrap().push((prompt.to_owned(), self.1.clone()));
        Ok(self.0.clone())
    }
}

impl loa_projects::Summarizers for Scripted {
    fn on(&self, model: Option<&str>) -> (Arc<dyn loa_projects::Summarizer>, Option<String>) {
        let answer = self.answers.lock().unwrap().remove(0);
        (Arc::new(Reply(answer, model.map(str::to_owned), self.asked.clone())), None)
    }
}

#[tokio::test]
async fn app_compaction_summarizes_with_the_small_model_and_continues_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = EchoAdapter::default();
    let lib = library(dir.path());
    two_turns(&lib, &adapter).await;
    let live = lib.live("s1").unwrap();
    let running = adapter.resume(config("s1"), &live.writer.with_dom(Dom::clone), live.writer.clone()).await.unwrap();
    let model = Arc::new(Scripted { answers: Mutex::new(vec!["They said first and second.".into()]), ..Default::default() });
    let smol = OneShot::new(model.clone());
    let source = live.writer.with_dom(Dom::clone);
    let next = loa_features::compact(&adapter, &lib, &smol, loa_features::Compaction {
        running: running.as_ref(), source: &source, source_sink: live.writer.as_ref(),
        next: config("s1-c"), smol_model: "@smol", instructions: Some("keep it short"),
    }).await.unwrap().unwrap();
    let (asked, role) = model.asked.lock().unwrap()[0].clone();
    assert_eq!(role.as_deref(), Some("@smol"));
    assert!(asked.contains("user: first") && asked.contains("keep it short"));
    assert!(next.config.context.unwrap().contains("They said first and second."));
    let dom = lib.load("s1").unwrap();
    let compaction = dom.iter().find(|n| n.kind == K::Compaction).unwrap();
    assert_eq!(compaction.text, "They said first and second.");
    assert_eq!(lib.load("s1-c").unwrap().root().unwrap().str_attr(attr::PARENT_SESSION), Some("s1"));
}

#[tokio::test]
async fn rules_and_advisor_run_around_turns() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(home.join(".omp/agent/rules")).unwrap();
    std::fs::create_dir_all(cwd.join(".omp/rules")).unwrap();
    std::fs::write(home.join(".omp/agent/rules/style.md"), "Use tabs.").unwrap();
    std::fs::write(cwd.join(".omp/rules/style.md"), "---\nalwaysApply: true\n---\nUse four spaces.").unwrap();
    std::fs::write(cwd.join(".omp/rules/db.md"), "---\ndescription: \"Database migrations\"\nglobs: migrations/**\n---\nNever drop columns.").unwrap();
    let rules = hooks::discover_rules(&cwd, &home);
    assert_eq!(rules.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["db", "style"]);
    let text = hooks::rules_context(&rules).unwrap();
    assert!(text.contains("Use four spaces.") && !text.contains("Use tabs."), "project overrides global: {text}");
    assert!(text.contains("db: Database migrations") && !text.contains("Never drop"), "{text}");

    let mut config = config("h1");
    let mut turn_hooks = TurnHooks::start(&mut config, Some(text.clone()));
    assert!(config.context.as_deref().unwrap().contains("Use four spaces."));
    assert_eq!(turn_hooks.before_turn(Some(text.clone()), "go"), "go");

    let adapter = EchoAdapter::default();
    let lib = library(&dir.path().join("store"));
    let live = lib.create("h1").unwrap();
    let session = adapter.start(config.clone(), live.writer.clone()).await.unwrap();
    session.send(prompt("go")).await.unwrap();
    let turn = lib.turn_seqs("h1").unwrap()[0].0;
    let model = Arc::new(Scripted { answers: Mutex::new(vec!["You ignored the spaces rule.".into(), "OK.".into()]), ..Default::default() });
    let advisor = Advisor::new(OneShot::new(model.clone()), "@smol");
    let dom = live.writer.with_dom(Dom::clone);
    let note = advisor.review(&dom, turn, turn_hooks.rules()).await.unwrap().unwrap();
    assert!(model.asked.lock().unwrap()[0].0.contains("user: go"));
    let message = hooks::record_note(live.writer.as_ref(), turn, &note).unwrap();
    let dom = live.writer.with_dom(Dom::clone);
    assert_eq!(dom.get(message).unwrap().str_attr(attr::BY), Some(hooks::ADVISOR));
    assert!(loa_features::turn_transcript(&dom, turn).contains("advisor: You ignored"));
    assert_eq!(advisor.review(&dom, turn, None).await.unwrap(), None, "OK means no note");

    turn_hooks.advise(note);
    let sent = turn_hooks.before_turn(Some("Use tabs only.".into()), "next");
    assert!(sent.starts_with("[The project rules changed]\nUse tabs only."), "{sent}");
    assert!(sent.contains("[Advisor note on your last turn]\nYou ignored") && sent.ends_with("next"), "{sent}");
    assert_eq!(turn_hooks.before_turn(Some("Use tabs only.".into()), "again"), "again");
}
