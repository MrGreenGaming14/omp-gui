//! Opt-in tests of fork, rewind, compaction, plan mode, model roles and the
//! advisor against the installed `omp`, with real model calls, through the
//! journal-backed library.
//! Run with `cargo test -p loa-features --test live_omp -- --ignored`.
//! `LOA_LIVE_MODEL` picks the model (default `anthropic/claude-haiku-5-5`).

use async_trait::async_trait;
use loa_adapter::{Adapter, Answer, Capabilities, Feature, HarnessSession, Prompt, SessionConfig, SessionState};
use loa_adapter_omp::{OmpAdapter, OmpAdapterConfig};
use loa_bus::{Authorize, Bus, Denied, NodeKey, SessionId};
use loa_core::{Dom, Draft, ElementKind, PatchSink, attr};
use loa_features::hooks::Advisor;
use loa_features::library::Library;
use loa_features::plan::{self, Outcome, PlanGate, Verdict};
use loa_features::smol::OneShot;
use loa_journal::{JournalOptions, Store};
use std::sync::Arc;
use std::time::Duration;

const TURN_TIMEOUT: Duration = Duration::from_secs(240);

fn model() -> String {
    std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".into())
}

struct Nobody;
impl Authorize for Nobody {
    fn authorize(&self, _: &SessionId, _: &NodeKey, _: &Draft, _: &Dom) -> Result<(), Denied> {
        Err(Denied("local only".into()))
    }
}

struct Env {
    _dir: tempfile::TempDir,
    cwd: std::path::PathBuf,
    lib: Library,
    adapter: OmpAdapter,
}

fn env() -> Env {
    env_with(None)
}

fn env_with(approval_mode: Option<&str>) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let store = Store::new(dir.path().join("sessions"), JournalOptions::default()).unwrap();
    let lib = Library::new(store, Arc::new(Bus::new()), NodeKey([3; 32]), Arc::new(Nobody));
    let mut config = OmpAdapterConfig::new("live-node", dir.path().join("state"));
    config.session_dir = Some(dir.path().join("omp-sessions"));
    config.approval_mode = approval_mode.map(str::to_owned);
    Env { cwd, lib, adapter: OmpAdapter::new(config), _dir: dir }
}

fn config(env: &Env, id: &str) -> SessionConfig {
    SessionConfig { session_id: id.into(), node: "live-node".into(), cwd: env.cwd.clone(), model: Some(model()), ..Default::default() }
}

async fn turn(session: &dyn HarnessSession, text: &str) {
    session.send(Prompt { text: text.into(), by: "live".into(), ..Default::default() }).await.expect("prompt accepted");
    tokio::time::timeout(TURN_TIMEOUT, session.wait_idle()).await.expect("turn finished in time").expect("turn ended");
}

/// Assistant text of the last turn.
fn last_reply(dom: &Dom) -> String {
    let root = dom.root().unwrap();
    let turn = dom.children_of_kind(root.id, ElementKind::Turn).last().expect("a turn");
    dom.children_of_kind(turn.id, ElementKind::Message)
        .filter(|m| m.str_attr(attr::ROLE) == Some("assistant") && m.str_attr(attr::KIND) != Some("thinking"))
        .map(|m| m.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn start(env: &Env, id: &str) -> Box<dyn HarnessSession> {
    let live = env.lib.create(id).unwrap();
    env.adapter.start(config(env, id), live.writer).await.expect("omp starts")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn a_real_omp_session_forks_with_its_history() {
    let env = env();
    let session = start(&env, "src").await;
    turn(session.as_ref(), "Remember this code word: PELICAN-42. Reply only with OK.").await;
    let source = env.lib.load("src").unwrap();
    let fork = loa_features::fork(&env.adapter, &env.lib, &source, config(&env, "branch")).await.expect("fork");
    turn(fork.harness.as_ref(), "What code word did I give you? Reply with only the code word.").await;
    let forked = env.lib.load("branch").unwrap();
    let reply = last_reply(&forked);
    assert!(reply.contains("PELICAN-42"), "the fork answered {reply:?}");
    let root = forked.root().unwrap();
    assert_eq!(root.str_attr(attr::PARENT_SESSION), Some("src"));
    assert_eq!(root.int_attr(attr::FORKED_AT), Some(source.seq() as i64));
    assert_eq!(env.lib.load("src").unwrap().seq(), source.seq(), "the source is untouched");
    fork.harness.shutdown().await.unwrap();
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn a_real_omp_session_rewinds_to_before_a_message() {
    let env = env();
    let session = start(&env, "src").await;
    turn(session.as_ref(), "The code word is HERON. Reply only with OK.").await;
    turn(session.as_ref(), "Forget that: the code word is now OTTER. Reply only with OK.").await;
    let turns = env.lib.turn_seqs("src").unwrap();
    assert_eq!(turns.len(), 2);
    let current = env.lib.load("src").unwrap();
    let target = env.lib.before_turn("src", turns[1].0).unwrap();
    let (plan, rewound) = loa_features::rewind(&env.adapter, &env.lib, &current, &target, config(&env, "rewound")).await.expect("rewind");
    assert!(plan.cancel.is_empty(), "nothing was running: {plan:?}");
    turn(rewound.harness.as_ref(), "What is the code word? Reply with only the code word.").await;
    let reply = last_reply(&env.lib.load("rewound").unwrap());
    assert!(reply.contains("HERON") && !reply.contains("OTTER"), "the rewound session answered {reply:?}");
    rewound.harness.shutdown().await.unwrap();
    session.shutdown().await.unwrap();
}

async fn until(lib: &Library, id: &str, what: &str, find: impl Fn(&Dom) -> bool) {
    tokio::time::timeout(TURN_TIMEOUT, async {
        while !find(&lib.load(id).unwrap()) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn a_real_omp_session_compacts_natively() {
    let env = env();
    let session = start(&env, "src").await;
    // omp keeps the newest ~20k tokens, so give it more than that to compact.
    let filler: String = (0..2500).map(|i| format!("Inventory note {i}: shelf {} holds {} crates of item {i}.\n", i % 40, i * 7 % 97)).collect();
    turn(session.as_ref(), &format!("Here are my warehouse notes:\n{filler}\nAlso, my favourite colour is teal. Reply only with OK.")).await;
    turn(session.as_ref(), "My favourite number is 7. Reply only with OK.").await;
    let live = env.lib.live("src").unwrap();
    let source = env.lib.load("src").unwrap();
    let smol = OneShot::new(Arc::new(loa_projects::OmpSummarizer::new("omp", &env.cwd)));
    let next = loa_features::compact(&env.adapter, &env.lib, &smol, loa_features::Compaction {
        running: session.as_ref(), source: &source, source_sink: live.writer.as_ref(),
        next: config(&env, "unused"), smol_model: "@smol", instructions: None,
    }).await.expect("compaction");
    assert!(next.is_none(), "omp compacts natively");
    until(&env.lib, "src", "the <compaction> element", |dom| dom.iter().any(|n| n.kind == ElementKind::Compaction)).await;
    let dom = env.lib.load("src").unwrap();
    let compaction = dom.iter().find(|n| n.kind == ElementKind::Compaction).unwrap();
    assert!(!compaction.text.trim().is_empty(), "omp wrote a summary");
    turn(session.as_ref(), "What is my favourite colour? Reply with one word.").await;
    let reply = last_reply(&env.lib.load("src").unwrap());
    assert!(reply.to_lowercase().contains("teal"), "after compaction omp answered {reply:?}");
    session.shutdown().await.unwrap();
}

/// The omp adapter without native compaction, so the app's fallback runs.
struct NoNativeCompaction<'a>(&'a OmpAdapter);

#[async_trait]
impl Adapter for NoNativeCompaction<'_> {
    fn harness(&self) -> &'static str { self.0.harness() }
    fn capabilities(&self) -> Capabilities {
        let mut caps = self.0.capabilities();
        caps.features.retain(|f| *f != Feature::Compaction);
        caps
    }
    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        self.0.start(config, sink).await
    }
    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        self.0.resume(config, dom, sink).await
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn the_app_compacts_with_a_real_smol_summary_into_a_fresh_omp_session() {
    let env = env();
    let session = start(&env, "src").await;
    turn(session.as_ref(), "I am planning a trip to Lisbon in May. Reply only with OK.").await;
    let live = env.lib.live("src").unwrap();
    let source = env.lib.load("src").unwrap();
    let smol = OneShot::new(Arc::new(loa_projects::OmpSummarizer::new("omp", &env.cwd)));
    let adapter = NoNativeCompaction(&env.adapter);
    let next = loa_features::compact(&adapter, &env.lib, &smol, loa_features::Compaction {
        running: session.as_ref(), source: &source, source_sink: live.writer.as_ref(),
        next: config(&env, "compacted"), smol_model: &model(), instructions: None,
    }).await.expect("compaction").expect("a fresh session");
    let dom = env.lib.load("src").unwrap();
    let compaction = dom.iter().find(|n| n.kind == ElementKind::Compaction).expect("the summary is recorded");
    assert!(compaction.text.contains("Lisbon"), "summary: {:?}", compaction.text);
    turn(next.harness.as_ref(), "Which city am I travelling to? Reply with one word.").await;
    let reply = last_reply(&env.lib.load("compacted").unwrap());
    assert!(reply.contains("Lisbon"), "the fresh session answered {reply:?}");
    assert_eq!(env.lib.load("compacted").unwrap().root().unwrap().str_attr(attr::PARENT_SESSION), Some("src"));
    next.harness.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn the_advisor_flags_a_broken_rule_with_a_real_model() {
    let env = env();
    let session = start(&env, "src").await;
    turn(session.as_ref(), "Write one sentence about fruit that uses the word banana.").await;
    let dom = env.lib.load("src").unwrap();
    let turn_node = env.lib.turn_seqs("src").unwrap()[0].0;
    let advisor = Advisor::new(OneShot::new(Arc::new(loa_projects::OmpSummarizer::new("omp", &env.cwd))), model());
    let note = advisor
        .review(&dom, turn_node, Some("Never use the word banana in any reply."))
        .await
        .expect("advisor call");
    let note = note.expect("the advisor flags the broken rule");
    assert!(note.to_lowercase().contains("banana"), "note: {note}");
    session.shutdown().await.unwrap();
}

/// Sends `text` and answers every tool approval of the turn with `decide`
/// until the session is idle.
async fn turn_answering(env: &Env, id: &str, session: &dyn HarnessSession, text: &str, decide: impl Fn(&loa_core::Node) -> Answer) {
    session.send(Prompt { text: text.into(), by: "live".into(), ..Default::default() }).await.expect("prompt accepted");
    tokio::time::timeout(TURN_TIMEOUT, async {
        let mut answered = std::collections::HashSet::new();
        loop {
            let dom = env.lib.load(id).unwrap();
            let waiting: Vec<_> = dom
                .iter()
                .filter(|n| n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == Some("waiting") && !answered.contains(&n.id))
                .cloned()
                .collect();
            for decision in waiting {
                answered.insert(decision.id);
                session.answer(decision.id, decide(&decision)).await.expect("answer accepted");
            }
            if session.state() == SessionState::Idle {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("turn finished in time");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn plan_mode_keeps_a_real_omp_session_read_only_until_the_plan_is_approved() {
    let env = env_with(Some("always-ask"));
    let session = start(&env, "plan").await;
    let live = env.lib.live("plan").unwrap();
    let gate = PlanGate::new();
    gate.enforce_through_approvals("plan");
    let dom = env.lib.load("plan").unwrap();
    let (permit, step) = loa_features::enter_plan(session.as_ref(), &dom, live.writer.as_ref(), &gate).await.expect("plan mode");
    let target = env.cwd.join("hello.txt");
    let ask = "Create the file hello.txt in the current directory containing the single line: hi there";
    turn_answering(&env, "plan", session.as_ref(), &plan::wrap_prompt(ask), |decision| {
        gate.verdict("plan", decision).expect("every approval in plan mode is denied")
    })
    .await;
    assert!(!target.exists(), "nothing was written in plan mode");
    let dom = env.lib.load("plan").unwrap();
    let turn_node = env.lib.turn_seqs("plan").unwrap()[0].0;
    let text = plan::plan_text(&dom, turn_node).expect("the agent wrote a plan").to_owned();
    let review = loa_features::submit_plan(live.writer.as_ref(), dom.root().unwrap().id, step, &text).unwrap();
    let dom = env.lib.load("plan").unwrap();
    let Outcome::Execute { prompt } = plan::answer_plan(session.as_ref(), &dom, live.writer.as_ref(), review, "live", Verdict::Approve, permit).await.unwrap() else {
        panic!("approval ends plan mode")
    };
    assert!(!gate.active("plan"));
    turn_answering(&env, "plan", session.as_ref(), &prompt, |decision| {
        assert!(gate.verdict("plan", decision).is_none());
        Answer { values: vec![loa_core::answer::ALLOW_ONCE.into()], reason: None, by: "live".into() }
    })
    .await;
    let written = std::fs::read_to_string(&target).expect("the approved plan wrote the file");
    assert!(written.contains("hi there"), "{written:?}");
    assert_eq!(env.lib.load("plan").unwrap().root().unwrap().str_attr(attr::MODE), Some("normal"));
    session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn a_model_role_switches_a_real_omp_session_to_its_first_available_model() {
    let env = env();
    let session = start(&env, "roles").await;
    let live = env.lib.live("roles").unwrap();
    let root = live.writer.with_dom(|d| d.root().unwrap().id);
    let candidates = vec!["nonexistent-provider/no-model".to_owned(), model()];
    let picked = loa_features::select_role(session.as_ref(), &env.adapter.capabilities(), live.writer.as_ref(), root, "smol", &candidates)
        .await
        .expect("a candidate is available");
    assert_eq!(picked, model());
    turn(session.as_ref(), "Reply with exactly the single word: pong").await;
    let dom = env.lib.load("roles").unwrap();
    assert_eq!(dom.root().unwrap().str_attr(attr::MODEL_ROLE), Some("smol"));
    assert!(last_reply(&dom).to_lowercase().contains("pong"));
    session.shutdown().await.unwrap();
}
