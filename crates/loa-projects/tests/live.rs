//! Opt-in test against the installed `omp`, with real model calls.
//! Run with `cargo test -p loa-projects --test live -- --ignored`.
//! `LOA_LIVE_MODEL` picks the chat model (default `anthropic/claude-haiku-5-5`);
//! digests use omp's `smol` role.

use loa_adapter::{Adapter, HarnessSession, Prompt, SessionConfig};
use loa_adapter_omp::{OmpAdapter, OmpAdapterConfig};
use loa_bus::{Authorize, Bus, Denied, NodeKey, Persist, PersistError, SessionId, SessionWriter, WriterConfig};
use loa_core::{Dom, Draft, ElementKind, Patch, attr};
use loa_journal::{Journal, JournalOptions, Store};
use loa_projects::{
    DigestState, FileSettingsStore, LiveSessions, LocalOnly, OmpSummarizer, PROJECT_SEARCH, ProjectService,
    ProjectSettings, RebuildOutcome, ServiceConfig, Trigger, digest_of,
};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const TURN_TIMEOUT: Duration = Duration::from_secs(240);

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

fn model() -> String {
    std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".into())
}

/// Opens `id` on the bus, persisted to its journal, and starts omp on it
/// with the project's host tools and `context`.
async fn start(
    bus: &Bus,
    store: &Store,
    service: &ProjectService,
    adapter: &OmpAdapter,
    cwd: &Path,
    id: &str,
    context: Option<String>,
) -> (Arc<SessionWriter>, Box<dyn HarnessSession>) {
    let writer = bus
        .open(WriterConfig {
            session: SessionId::new(id),
            owner: NodeKey([7; 32]),
            local: NodeKey([7; 32]),
            dom: Dom::new(),
            persist: Box::new(JournalPersist(store.create(id).unwrap())),
            authorize: Arc::new(AllowAll),
        })
        .unwrap();
    let (host_tools, handler) = service.host_tools();
    let config = SessionConfig {
        session_id: id.into(),
        node: "live-test".into(),
        cwd: cwd.to_owned(),
        project: Some("liveproj".into()),
        model: Some(model()),
        context,
        host_tools,
        host_tool_handler: Some(handler),
        ..Default::default()
    };
    let session = adapter.start(config, writer.clone()).await.expect("omp starts in rpc mode");
    (writer, session)
}

async fn turn(session: &dyn HarnessSession, text: &str) {
    session.send(Prompt { text: text.into(), by: "live-test".into(), ..Default::default() }).await.expect("prompt accepted");
    tokio::time::timeout(TURN_TIMEOUT, session.wait_idle()).await.expect("turn finished in time").expect("turn ended");
}

fn assistant_text(dom: &Dom) -> String {
    dom.iter()
        .filter(|n| {
            n.kind == ElementKind::Message
                && n.str_attr(attr::ROLE) == Some("assistant")
                && n.str_attr(attr::KIND) != Some("thinking")
        })
        .map(|n| n.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn a_digest_from_one_session_reaches_the_next_and_project_search_works() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("liveproj");
    std::fs::create_dir_all(&cwd).unwrap();
    let store = Store::new(dir.path().join("sessions"), JournalOptions::default()).unwrap();
    let bus = Arc::new(Bus::new());
    let settings = Arc::new(FileSettingsStore::new(dir.path().join("projects")));
    let summarizer = Arc::new(OmpSummarizer::new("omp", dir.path()));
    let config = ServiceConfig { debounce: Duration::from_millis(50), ..ServiceConfig::default() };
    let (service, mut outcomes) = ProjectService::start(
        LiveSessions::new(bus.clone(), store.clone()),
        settings,
        Arc::new(LocalOnly),
        summarizer,
        config,
    );
    service
        .save_settings("liveproj", &ProjectSettings { instructions: "Answer briefly.".into(), ..Default::default() })
        .await
        .unwrap();
    let adapter = OmpAdapter::new(OmpAdapterConfig::new("live-test", dir.path().join("omp-state")));

    // Session 1 makes a decision; its turn end rebuilds the digest with omp's smol role.
    let (first, session) = start(&bus, &store, &service, &adapter, &cwd, "first", None).await;
    turn(
        session.as_ref(),
        "We decided: the control port is 31415 and the code lives in src/net.rs. Reply only with: noted.",
    )
    .await;
    session.shutdown().await.unwrap();
    assert!(service.notify("first", Trigger::TurnEnd));
    let outcome = tokio::time::timeout(TURN_TIMEOUT, outcomes.recv()).await.expect("rebuild in time").unwrap();
    assert!(matches!(outcome, RebuildOutcome::Rebuilt { .. }), "{outcome:?}");
    let digest = first.with_dom(digest_of).expect("a digest was written");
    eprintln!("digest: {:?} (model {:?})", digest.content, digest.model);
    assert_eq!(digest.model.as_deref(), Some("smol"));
    assert!(!digest.stale);
    let overview = service.overview("liveproj").await.unwrap();
    assert_eq!(overview.sessions[0].digest_state, DigestState::Fresh);

    // Session 2 starts with the digest in its context and searches the project.
    let context = service.context("liveproj", Some("second"), "control port").await.unwrap().expect("project context");
    assert!(context.contains("Answer briefly."));
    assert!(context.contains(&digest.content.summary), "{context}");
    let (second, session) = start(&bus, &store, &service, &adapter, &cwd, "second", Some(context)).await;
    turn(
        session.as_ref(),
        "First, from the project context you were given, which control port did the other session pick? \
         Then call the project_search tool with query \"31415\" and tell me the session id of the first hit.",
    )
    .await;
    session.shutdown().await.unwrap();
    let dom = second.with_dom(Dom::clone);
    let reply = assistant_text(&dom);
    eprintln!("reply: {reply}");
    assert!(reply.contains("31415"), "assistant said {reply:?}");
    let call = dom
        .iter()
        .find(|n| n.kind == ElementKind::Call && n.str_attr(attr::TOOL) == Some(PROJECT_SEARCH))
        .expect("the model called project_search");
    let result = dom.children_of_kind(call.id, ElementKind::Result).next().expect("project_search has a result");
    eprintln!("project_search result: {}", result.text);
    assert!(result.text.contains("\"session\":\"first\""), "{}", result.text);
    assert!(reply.contains("first"), "assistant said {reply:?}");
}
