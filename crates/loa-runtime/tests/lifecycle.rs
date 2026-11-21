//! The headless lifecycle on a scripted harness: effects are awaited and
//! their results dispatched back as `Command::Completed`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use loa_bus::{Authorize, Bus, Denied, Event, JournalOptions, NodeKey, Policy, SessionId, SessionWriter, Store};
use loa_contracts::harness::{
    Adapter, AdapterError, Answer, Capabilities, HarnessSession, HarnessTraits, Prompt, Result as HResult, SessionConfig, SessionState,
};
use loa_core::{Dom, Draft, ElementKind, NodeId, PatchSink, Role, TurnStatus, Value, attr};
use loa_harness::testing::ScriptedAdapter;
use loa_runtime::feed::Source;
use loa_runtime::harnesses::{Harness, Harnesses};
use loa_runtime::lifecycle::{Command, Completion, Effect, OperationId, ResumeError, RuntimeEvent, Transition};
use loa_runtime::session::{Feed, SessionEvent, WriterFeed};
use loa_runtime::{Runtime, RuntimeParts};
use loa_session::library::Library;

const HARNESS: &str = "scripted";

struct Nobody;
impl Authorize for Nobody {
    fn authorize(&self, _: &SessionId, _: &NodeKey, _: &Draft, _: &Dom) -> Result<(), Denied> {
        Err(Denied("local only".into()))
    }
}

fn library(dir: &Path) -> Arc<Library> {
    Arc::new(Library::new(Store::new(dir, JournalOptions::default()).unwrap(), Arc::new(Bus::new()), NodeKey([1; 32]), Arc::new(Nobody)))
}

/// The scripted harness, counting how often its sessions shut down.
#[derive(Clone)]
struct Counted {
    inner: ScriptedAdapter,
    shutdowns: Arc<AtomicUsize>,
    /// Shutdowns fail once set.
    fail_shutdown: Arc<AtomicBool>,
}

impl Counted {
    fn new(inner: ScriptedAdapter) -> Self {
        Self { inner, shutdowns: Arc::default(), fail_shutdown: Arc::default() }
    }

    fn shutdowns(&self) -> usize {
        self.shutdowns.load(Ordering::SeqCst)
    }

    fn wrap(&self, session: Box<dyn HarnessSession>) -> Box<dyn HarnessSession> {
        Box::new(CountedSession { inner: session, shutdowns: Arc::clone(&self.shutdowns), fail_shutdown: Arc::clone(&self.fail_shutdown) })
    }
}

#[async_trait]
impl Adapter for Counted {
    fn harness(&self) -> &'static str {
        self.inner.harness()
    }

    fn capabilities(&self) -> Capabilities {
        Adapter::capabilities(&self.inner)
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> HResult<Box<dyn HarnessSession>> {
        self.inner.start(config, sink).await.map(|session| self.wrap(session))
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> HResult<Box<dyn HarnessSession>> {
        self.inner.resume(config, dom, sink).await.map(|session| self.wrap(session))
    }
}

struct CountedSession {
    inner: Box<dyn HarnessSession>,
    shutdowns: Arc<AtomicUsize>,
    fail_shutdown: Arc<AtomicBool>,
}

#[async_trait]
impl HarnessSession for CountedSession {
    async fn send(&self, prompt: Prompt) -> HResult<()> {
        self.inner.send(prompt).await
    }

    async fn interrupt(&self) -> HResult<()> {
        self.inner.interrupt().await
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> HResult<()> {
        self.inner.answer(decision, answer).await
    }

    fn harness(&self) -> &'static str {
        self.inner.harness()
    }

    fn state(&self) -> SessionState {
        self.inner.state()
    }

    async fn wait_idle(&self) -> HResult<()> {
        self.inner.wait_idle().await
    }

    async fn shutdown(&self) -> HResult<()> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        if self.fail_shutdown.load(Ordering::SeqCst) {
            return Err(AdapterError::Protocol("shutdown failed".to_owned()));
        }
        self.inner.shutdown().await
    }
}

fn runtime(library: Arc<Library>, adapter: &Counted, cwd: &Path) -> Runtime {
    let mut harnesses = Harnesses::default();
    let harness = Harness {
        id: HARNESS.to_owned(),
        label: "Scripted".to_owned(),
        available: true,
        detail: None,
        executable: None,
        configured: None,
        availability: None,
        traits: HarnessTraits::default(),
        program: "scripted",
    };
    harnesses.add(Arc::new(adapter.clone()), harness);
    let mut runtime = Runtime::new(RuntimeParts {
        library,
        harnesses,
        node: "node".to_owned(),
        cwd: cwd.to_path_buf(),
        project: "project".to_owned(),
        access: None,
    });
    runtime.sessions_mut().harness = Some(HARNESS.to_owned());
    runtime
}

fn prompt(text: &str) -> Prompt {
    Prompt { text: text.to_owned(), attachments: Vec::new(), by: "user".to_owned() }
}

/// The single operation a start, resume or spawn prepared, with its base config.
fn prepared(transition: &Transition) -> (OperationId, Box<SessionConfig>) {
    assert!(transition.effects.is_empty(), "nothing launches before the config is complete");
    match transition.events.as_slice() {
        [RuntimeEvent::PrepareSession { operation, config, harness, .. }] => {
            assert_eq!(harness, HARNESS);
            (*operation, config.clone())
        }
        _ => panic!("expected one PrepareSession"),
    }
}

/// Runs the transition's only effect.
async fn run_one(transition: Transition) -> Completion {
    let [effect]: [Effect; 1] = transition.effects.try_into().unwrap_or_else(|effects: Vec<Effect>| panic!("{} effects", effects.len()));
    effect.await
}

/// Starts a session through the whole handshake and returns its id and
/// the transition its start completed with.
async fn start(runtime: &mut Runtime, prompt: Option<Prompt>) -> (SessionId, Transition) {
    let (operation, config) = prepared(&runtime.dispatch(Command::Start { prompt }));
    let id = SessionId::from(config.session_id.as_str());
    let completion = run_one(runtime.dispatch(Command::Prepared { operation, config })).await;
    (id, runtime.dispatch(Command::Completed(completion)))
}

fn writer(runtime: &Runtime, id: &SessionId) -> Arc<SessionWriter> {
    Arc::clone(runtime.sessions().get(id.as_str()).unwrap().writer())
}

#[tokio::test]
async fn start_sends_the_prompt_once_and_reopens_from_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = Counted::new(ScriptedAdapter::new(HARNESS));
    let mut runtime = runtime(library(dir.path()), &adapter, dir.path());

    let (id, started) = start(&mut runtime, Some(prompt("hello"))).await;
    assert!(started.effects.is_empty(), "the prompt waits for the app's before_send");
    let mut requested = Vec::new();
    match started.events.as_slice() {
        [
            RuntimeEvent::Session(SessionEvent::Activated(Some(activated))),
            RuntimeEvent::SessionStarted(running),
            RuntimeEvent::SendRequested { id: send_to, prompt },
        ] => {
            assert_eq!((activated, running, send_to), (&id, &id, &id));
            requested.push(prompt.clone());
        }
        _ => panic!("expected activation, start, then one send request"),
    }
    assert!(runtime.sessions().is_active(id.as_str()));
    assert!(runtime.sessions().starting().is_none());

    let [prompt] = <[Prompt; 1]>::try_from(requested).unwrap();
    let send = runtime.dispatch(Command::Send(prompt));
    assert!(matches!(send.events.as_slice(), [RuntimeEvent::TurnStarted(turn)] if *turn == id));
    let done = run_one(send).await;
    assert!(matches!(done, Completion::Done(Ok(()))));
    assert!(runtime.dispatch(Command::Completed(done)).events.is_empty());
    assert_eq!(adapter.inner.seen().prompts, ["hello"]);

    let written = writer(&runtime, &id).with_dom(Dom::clone);
    assert!(written.seq() > 0);
    let closed = runtime.dispatch(Command::Close { id: id.clone(), delete: false });
    assert!(matches!(
        closed.events.as_slice(),
        [RuntimeEvent::Session(SessionEvent::Activated(None)), RuntimeEvent::Session(SessionEvent::Closed(gone))] if *gone == id
    ));
    assert!(matches!(run_one(closed).await, Completion::ShutDown(Ok(()))));
    assert_eq!(adapter.shutdowns(), 1);

    let opened = runtime.dispatch(Command::Open(id.clone()));
    assert!(matches!(opened.events.as_slice(), [RuntimeEvent::Session(SessionEvent::Activated(Some(active)))] if *active == id));
    let reopened = runtime.sessions().get(id.as_str()).unwrap().dom();
    assert_eq!(reopened.seq(), written.seq());
    assert_eq!(*reopened, written);
    assert_eq!(adapter.inner.seen().prompts.len(), 1, "reopening sends nothing");
}

#[tokio::test]
async fn a_start_completing_after_close_shuts_its_harness_down() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = Counted::new(ScriptedAdapter::new(HARNESS));
    let mut runtime = runtime(library(dir.path()), &adapter, dir.path());

    let (operation, config) = prepared(&runtime.dispatch(Command::Start { prompt: Some(prompt("hello")) }));
    let id = SessionId::from(config.session_id.as_str());
    let completion = run_one(runtime.dispatch(Command::Prepared { operation, config })).await;
    assert!(matches!(&completion, Completion::Started { result: Ok(_), .. }));

    let closed = runtime.dispatch(Command::Close { id: id.clone(), delete: true });
    assert!(closed.effects.is_empty(), "no harness was attached yet");
    assert!(matches!(closed.events.as_slice(), [RuntimeEvent::Session(SessionEvent::Closed(gone))] if *gone == id));

    let late = runtime.dispatch(Command::Completed(completion));
    assert!(late.events.is_empty(), "nothing activates: {} events", late.events.len());
    assert!(matches!(run_one(late).await, Completion::ShutDown(Ok(()))));
    assert_eq!(adapter.shutdowns(), 1);
    assert!(runtime.sessions().active_id().is_none());
    assert!(runtime.sessions().get(id.as_str()).is_none());
    assert!(runtime.sessions().starting().is_none());
    assert!(adapter.inner.seen().prompts.is_empty());
}

#[tokio::test]
async fn a_failed_shutdown_of_a_closed_session_leaves_the_active_turn_busy() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = Counted::new(ScriptedAdapter::new(HARNESS));
    let mut runtime = runtime(library(dir.path()), &adapter, dir.path());

    let (closing, _) = start(&mut runtime, None).await;
    let (active, _) = start(&mut runtime, None).await;
    assert!(runtime.sessions().is_active(active.as_str()));
    // The active session's turn runs; its send is never awaited.
    let send = runtime.dispatch(Command::Send(prompt("hello")));
    assert_eq!(send.effects.len(), 1);
    assert!(runtime.sessions().active().unwrap().is_busy());

    adapter.fail_shutdown.store(true, Ordering::SeqCst);
    let closed = runtime.dispatch(Command::Close { id: closing.clone(), delete: false });
    assert!(matches!(closed.events.as_slice(), [RuntimeEvent::Session(SessionEvent::Closed(gone))] if *gone == closing));
    let shut = run_one(closed).await;
    assert!(matches!(&shut, Completion::ShutDown(Err(_))));
    let failed = runtime.dispatch(Command::Completed(shut));
    assert!(failed.effects.is_empty());
    assert!(matches!(failed.events.as_slice(), [RuntimeEvent::Notice(notice)] if notice.contains("shutdown failed")));
    assert!(runtime.sessions().is_active(active.as_str()));
    assert!(runtime.sessions().active().unwrap().is_busy(), "another session's shutdown doesn't end the active turn");
}

#[tokio::test]
async fn a_second_prepared_launches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = Counted::new(ScriptedAdapter::new(HARNESS));
    let mut runtime = runtime(library(dir.path()), &adapter, dir.path());

    let (operation, config) = prepared(&runtime.dispatch(Command::Start { prompt: None }));
    let first = runtime.dispatch(Command::Prepared { operation, config: config.clone() });
    assert_eq!(first.effects.len(), 1);
    let second = runtime.dispatch(Command::Prepared { operation, config: config.clone() });
    assert!(second.effects.is_empty() && second.events.is_empty());
    let unknown = runtime.dispatch(Command::Prepared { operation: operation + 1, config });
    assert!(unknown.effects.is_empty() && unknown.events.is_empty());

    let completion = run_one(first).await;
    let started = runtime.dispatch(Command::Completed(completion));
    assert!(matches!(started.events.as_slice(), [RuntimeEvent::Session(SessionEvent::Activated(Some(_))), RuntimeEvent::SessionStarted(_)]));
    assert_eq!(adapter.inner.seen().configs.len(), 1, "the harness started once");
}

#[tokio::test]
async fn resume_errors_continue_or_notify() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old");
    let new = dir.path().join("new");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&new).unwrap();
    let adapter = Counted::new(ScriptedAdapter::new(HARNESS).no_resume());
    let mut runtime = runtime(library(&dir.path().join("library")), &adapter, &old);

    // A session in `old` without a running harness.
    let (id, _) = start(&mut runtime, None).await;
    let closed = runtime.dispatch(Command::Close { id: id.clone(), delete: false });
    run_one(closed).await;
    runtime.dispatch(Command::Open(id.clone()));
    assert!(runtime.sessions().active().unwrap().harness().is_none());
    runtime.set_active("other".to_owned(), new.clone());

    let (operation, config) = prepared(&runtime.dispatch(Command::Send(prompt("again"))));
    assert_eq!(config.cwd, old, "a resume runs in the session's own folder");
    let completion = run_one(runtime.dispatch(Command::Prepared { operation, config })).await;
    assert!(matches!(&completion, Completion::Resumed { result: Err(ResumeError::Unsupported(_)), .. }));
    let unsupported = runtime.dispatch(Command::Completed(completion));
    assert!(unsupported.effects.is_empty());
    match unsupported.events.as_slice() {
        [RuntimeEvent::Notice(_), RuntimeEvent::ContinueRequested { folder, prompt }] => {
            assert_eq!(folder.as_deref(), Some(old.as_path()));
            assert_eq!(prompt.text, "again");
        }
        _ => panic!("expected a notice and a continue request"),
    }

    let (operation, config) = prepared(&runtime.dispatch(Command::Send(prompt("once more"))));
    // The harness call is never run: its failure comes back as is.
    drop(runtime.dispatch(Command::Prepared { operation, config }));
    let failed = runtime.dispatch(Command::Completed(Completion::Resumed {
        operation,
        id: id.clone(),
        result: Err(ResumeError::Failed("boom".to_owned())),
        prompt: prompt("once more"),
    }));
    assert!(failed.effects.is_empty());
    match failed.events.as_slice() {
        [RuntimeEvent::Notice(notice), RuntimeEvent::ResumeFailed(session)] => {
            assert_eq!(notice, "boom");
            assert_eq!(*session, id);
        }
        _ => panic!("expected only the notice and the composer's turn ending"),
    }
    assert!(runtime.sessions().starting().is_none());
}

#[tokio::test]
async fn feed_skips_stale_patches_merges_appends_resyncs_and_ends() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = Counted::new(ScriptedAdapter::new(HARNESS));
    let mut runtime = runtime(library(dir.path()), &adapter, dir.path());
    let (id, _) = start(&mut runtime, None).await;
    let writer = writer(&runtime, &id);
    let root = writer.with_dom(|dom| dom.root().unwrap().id);
    let turn = writer.commit(Draft::insert(root, ElementKind::Turn, [(attr::STATUS, Value::from(TurnStatus::Done))], "")).unwrap().node;
    let mut feed = WriterFeed::new(Arc::clone(&writer), Policy::coalescing(3));
    let dispatch = |runtime: &mut Runtime, feed: Feed| runtime.dispatch(Command::Feed { id: id.clone(), feed }).events;

    // The feed starts from the writer's DOM, which is ahead of the runtime's copy.
    let synced = feed.next().await.unwrap();
    assert!(matches!(synced, Feed::Synced(_)));
    assert!(matches!(dispatch(&mut runtime, synced.clone()).as_slice(), [RuntimeEvent::Session(SessionEvent::Loaded(_))]));
    assert!(dispatch(&mut runtime, synced).is_empty(), "the same DOM again changes nothing");

    let message = writer.commit(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from(Role::Assistant))], "")).unwrap().node;
    for text in ["one ", "two ", "three"] {
        writer.commit(Draft::append(message, text)).unwrap();
    }
    let inserted = feed.next().await.unwrap();
    let events = dispatch(&mut runtime, inserted.clone());
    assert!(matches!(events.as_slice(), [RuntimeEvent::Session(SessionEvent::Patch(..))]));
    assert!(dispatch(&mut runtime, inserted).is_empty(), "a stale patch is ignored");

    let appended = feed.next().await.unwrap();
    assert!(matches!(&appended, Feed::Event(Event::Appends(appends)) if appends.last_seq > appends.first_seq));
    let events = dispatch(&mut runtime, appended);
    assert!(matches!(events.as_slice(), [RuntimeEvent::Session(SessionEvent::Patch(..))]));
    let dom = runtime.sessions().get(id.as_str()).unwrap().dom();
    assert_eq!(dom.get(message).unwrap().text, "one two three");
    assert_eq!(dom.seq(), writer.with_dom(Dom::seq));

    // More than the subscription holds: it lags and resubscribes from the writer's DOM.
    for _ in 0..5 {
        writer.commit(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from(Role::User))], "hi")).unwrap();
    }
    let resynced = loop {
        let next = feed.next().await.unwrap();
        let resynced = matches!(next, Feed::Synced(_));
        let events = dispatch(&mut runtime, next);
        if resynced {
            break events;
        }
    };
    assert!(
        matches!(resynced.as_slice(), [RuntimeEvent::Session(SessionEvent::Loaded(session))] if *session == id),
        "the resync replaces the DOM"
    );
    assert_eq!(*runtime.sessions().get(id.as_str()).unwrap().dom(), writer.with_dom(Dom::clone));

    writer.close();
    let ended = feed.next().await.unwrap();
    assert!(matches!(ended, Feed::Ended));
    assert!(dispatch(&mut runtime, ended).is_empty());
    assert!(feed.next().await.is_none(), "nothing follows the end");
    assert_eq!(*runtime.sessions().get(id.as_str()).unwrap().dom(), writer.with_dom(Dom::clone));
}
