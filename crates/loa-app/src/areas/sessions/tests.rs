//! The sessions area driven through `App::update`, on a fake harness.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use loa_adapter::{
    Adapter, AdapterError, Answer, Capabilities, HarnessSession, ModelSelection, ModelSwitch, Prompt, Registry, SessionConfig,
    SessionState,
};
use loa_bus::{Policy, SessionId};
use loa_core::{
    DecisionKind, DecisionStatus, Dom, Draft, ElementKind, NodeId, PatchSink, Role, SessionMode, TurnStatus, Value, answer,
    attr, now_ms,
};
use loa_identity::NodeIdentity;
use loa_ui_composer as composer;
use loa_ui_palette as palette;
use loa_ui_panel::{agents, session_tree};
use loa_ui_shell as shell;

use super::Message as SessionsMessage;
use crate::app::tests::{active, prompt, run, update};
use crate::app::{App, Message};
use crate::services::{Dirs, Harnesses, Node, Services};
use crate::session::Feed;

/// What the fake harness saw.
#[derive(Default)]
struct Seen {
    configs: Vec<SessionConfig>,
    prompts: Vec<String>,
    models: Vec<String>,
}

type Shared = Arc<Mutex<Seen>>;

/// A harness that answers each prompt at once with `reply`. A prompt
/// containing "ask" waits on a tool approval first. Imports read a file of
/// prompts, one per line, as turns; exports write them back.
struct TestAdapter {
    id: &'static str,
    seen: Shared,
    reply: Arc<Mutex<String>>,
}

#[async_trait]
impl Adapter for TestAdapter {
    fn harness(&self) -> &'static str {
        self.id
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { model_switch: ModelSwitch::PerCall, import: true, export: true, ..Capabilities::default() }
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        let root = sink.commit(root_draft(self.id, &config.session_id, &config.node))?.node;
        self.seen.lock().unwrap().configs.push(config);
        Ok(Box::new(TestSession::new(self, sink, root)))
    }

    async fn resume(&self, config: SessionConfig, dom: &Dom, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        let root = dom.root().map(|root| root.id).ok_or_else(|| AdapterError::Format("empty DOM".into()))?;
        self.seen.lock().unwrap().configs.push(config);
        Ok(Box::new(TestSession::new(self, sink, root)))
    }

    fn import(&self, native: &Path, sink: &dyn PatchSink) -> loa_adapter::Result<()> {
        let text = std::fs::read_to_string(native).map_err(|error| AdapterError::Format(error.to_string()))?;
        let root = sink.commit(root_draft(self.id, "native-imported", "elsewhere"))?.node;
        for line in text.lines().skip(1) {
            let turn = sink.commit(Draft::insert(root, ElementKind::Turn, [("status", Value::from(TurnStatus::Done))], ""))?.node;
            sink.commit(Draft::insert(turn, ElementKind::Message, [("role", Value::from(Role::User))], line))?;
        }
        Ok(())
    }

    fn export(&self, dom: &Dom, out: &Path) -> loa_adapter::Result<()> {
        let lines: Vec<&str> = dom
            .iter()
            .filter(|node| node.kind == ElementKind::Message && node.str_attr(attr::ROLE) == Some("user"))
            .map(|node| node.text.as_str())
            .collect();
        std::fs::write(out, format!("{{\"type\":\"session\"}}\n{}\n", lines.join("\n"))).map_err(|error| AdapterError::Format(error.to_string()))
    }
}

fn root_draft(harness: &str, id: &str, node: &str) -> Draft {
    let attrs: [(&str, Value); 4] =
        [("id", id.into()), ("node", node.into()), ("harness", harness.into()), ("created", Value::Int(now_ms()))];
    Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, "")
}

struct TestSession {
    id: &'static str,
    sink: Arc<dyn PatchSink>,
    root: NodeId,
    seen: Shared,
    reply: Arc<Mutex<String>>,
    waiting: Mutex<Option<NodeId>>,
}

impl TestSession {
    fn new(adapter: &TestAdapter, sink: Arc<dyn PatchSink>, root: NodeId) -> Self {
        Self {
            id: adapter.id,
            sink,
            root,
            seen: Arc::clone(&adapter.seen),
            reply: Arc::clone(&adapter.reply),
            waiting: Mutex::default(),
        }
    }

    fn end(&self, turn: NodeId) -> loa_adapter::Result<()> {
        let reply = self.reply.lock().unwrap().clone();
        let attrs: [(&str, Value); 2] = [("role", Role::Assistant.into()), ("status", "done".into())];
        self.sink.commit(Draft::insert(turn, ElementKind::Message, attrs, reply.as_str()))?;
        self.sink.commit(Draft::set(turn, "status", TurnStatus::Done))?;
        Ok(())
    }
}

#[async_trait]
impl HarnessSession for TestSession {
    async fn send(&self, prompt: Prompt) -> loa_adapter::Result<()> {
        self.seen.lock().unwrap().prompts.push(prompt.text.clone());
        let turn = self.sink.commit(Draft::insert(self.root, ElementKind::Turn, [("status", Value::from(TurnStatus::Running))], ""))?.node;
        self.sink.commit(Draft::insert(turn, ElementKind::Message, [("role", Value::from(Role::User))], prompt.text.as_str()))?;
        if prompt.text.contains("ask") {
            let attrs: [(&str, Value); 3] =
                [("kind", DecisionKind::Approval.into()), ("status", DecisionStatus::Waiting.into()), ("title", "Write a.txt".into())];
            self.sink.commit(Draft::insert(turn, ElementKind::Decision, attrs, "write a.txt"))?;
            self.sink.commit(Draft::set(turn, "status", TurnStatus::Waiting))?;
            *self.waiting.lock().unwrap() = Some(turn);
            return Ok(());
        }
        self.end(turn)
    }

    async fn interrupt(&self) -> loa_adapter::Result<()> {
        Ok(())
    }

    async fn answer(&self, decision: NodeId, answer: Answer) -> loa_adapter::Result<()> {
        let values = Value::List(answer.values.into_iter().map(Value::from).collect());
        self.sink.commit(Draft::set(decision, "answer", values))?;
        self.sink.commit(Draft::set(decision, "status", DecisionStatus::Answered))?;
        let turn = self.waiting.lock().unwrap().take().ok_or(AdapterError::UnknownDecision(decision))?;
        self.sink.commit(Draft::set(turn, "status", TurnStatus::Running))?;
        self.end(turn)
    }

    async fn set_model(&self, selection: ModelSelection) -> loa_adapter::Result<()> {
        if selection.model.starts_with("gone/") {
            return Err(AdapterError::Format(format!("unknown model {}", selection.model)));
        }
        self.seen.lock().unwrap().models.push(selection.model);
        Ok(())
    }

    fn harness(&self) -> &'static str {
        self.id
    }

    fn state(&self) -> SessionState {
        SessionState::Idle
    }

    async fn wait_idle(&self) -> loa_adapter::Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> loa_adapter::Result<()> {
        Ok(())
    }
}

/// A small model that answers every call with its `answer` and records the
/// prompts.
struct FakeModel {
    answer: String,
    prompts: Mutex<Vec<String>>,
}

#[async_trait]
impl loa_projects::Summarizer for FakeModel {
    async fn summarize(&self, prompt: &str, _: Option<&str>) -> loa_projects::Result<loa_projects::SummaryAnswer> {
        self.prompts.lock().unwrap().push(prompt.to_owned());
        Ok(loa_projects::SummaryAnswer { text: self.answer.clone(), model: None })
    }
}

struct Env {
    dir: tempfile::TempDir,
    app: App,
    seen: Shared,
    reply: Arc<Mutex<String>>,
    /// Every open session's feed, as the app's subscription delivers it.
    feeds: Vec<(SessionId, loa_bus::Subscription)>,
}

impl Env {
    /// The app on one fake harness `id`, with `home` under the temp dir.
    fn new(id: &'static str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let seen = Shared::default();
        let reply = Arc::new(Mutex::new("ok".to_owned()));
        let app = build(dir.path(), id, &seen, &reply);
        Self { dir, app, seen, reply, feeds: Vec::new() }
    }

    fn home(&self) -> std::path::PathBuf {
        self.dir.path().join("home")
    }

    fn small_model(&mut self, answer: &str) -> Arc<FakeModel> {
        let model = Arc::new(FakeModel { answer: answer.to_owned(), prompts: Mutex::default() });
        self.app.areas.sessions.set_small_model(Arc::clone(&model) as _);
        model
    }

    /// Delivers every open session's patches, subscribing to sessions that
    /// opened since, until nothing is left.
    fn pump(&mut self) {
        loop {
            let new: Vec<_> = self
                .app
                .ctx
                .sessions
                .iter()
                .filter(|session| !self.feeds.iter().any(|(id, _)| id == session.id()))
                .map(|session| (session.id().clone(), Arc::clone(session.writer())))
                .collect();
            for (id, writer) in new {
                let (dom, feed) = writer.subscribe(Policy::every_patch(4096));
                self.feeds.push((id.clone(), feed));
                update(&mut self.app, Message::Session(id, Feed::Synced(Arc::new(dom))));
            }
            let mut idle = true;
            for index in 0..self.feeds.len() {
                while let Ok(event) = self.feeds[index].1.try_recv() {
                    idle = false;
                    let id = self.feeds[index].0.clone();
                    update(&mut self.app, Message::Session(id, Feed::Event(event)));
                }
            }
            if idle {
                return;
            }
        }
    }

    fn send(&mut self, message: Message) {
        update(&mut self.app, message);
        self.pump();
    }

    fn say(&mut self, text: &str) {
        prompt(&mut self.app, text);
        self.pump();
    }

    /// Starts a session and returns its id.
    fn start(&mut self) -> SessionId {
        self.send(Message::NewSession);
        self.app.ctx.sessions.active().expect("started").id().clone()
    }

    fn command(&mut self, id: &str) {
        let task = self.app.update(Message::Palette(palette::Message::Open));
        run(&mut self.app, task);
        let task = self.app.command(&palette::CommandId::new(id));
        run(&mut self.app, task);
        self.pump();
    }

    fn active_dom(&self) -> Dom {
        self.app.ctx.sessions.active().unwrap().writer().with_dom(Dom::clone)
    }

    fn last_config(&self) -> SessionConfig {
        self.seen.lock().unwrap().configs.last().cloned().expect("a session started")
    }
}

fn build(dir: &Path, id: &'static str, seen: &Shared, reply: &Arc<Mutex<String>>) -> App {
    let mut harnesses = Harnesses { registry: Registry::new(), list: Vec::new() };
    let adapter = TestAdapter { id, seen: Arc::clone(seen), reply: Arc::clone(reply) };
    harnesses.add(Arc::new(adapter), id, true, None, None);
    let node = Node::new(NodeIdentity::from_secret_bytes(&[7; 32]), "testhost".into());
    let services = Services::new(node, Dirs::new(dir.join("loa")), harnesses, dir.to_owned()).unwrap();
    let mut app = App::build(services).0;
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    app.areas.sessions.set_home(home);
    app
}

/// The first node of `kind` under the active session's root, in order, with
/// `role` when given.
fn user_messages(dom: &Dom) -> Vec<(NodeId, String)> {
    dom.iter()
        .filter(|node| node.kind == ElementKind::Message && node.str_attr(attr::ROLE) == Some("user"))
        .map(|node| (node.id, node.text.clone()))
        .collect()
}

fn root_attr(dom: &Dom, name: &str) -> Option<String> {
    dom.root().and_then(|root| root.attr(name)).map(|value| match value {
        Value::Int(int) => int.to_string(),
        other => other.as_str().unwrap_or_default().to_owned(),
    })
}

#[test]
fn slash_compact_is_the_areas_command() {
    assert!(matches!(super::slash("/compact"), Some(SessionsMessage::Compact(None))));
    assert!(matches!(super::slash("  /compact keep the names "), Some(SessionsMessage::Compact(Some(i))) if i == "keep the names"));
    assert!(super::slash("/compacted").is_none());
    assert!(super::slash("compact").is_none());
}

#[test]
fn deleting_a_recent_removes_it_and_its_journal() {
    let mut env = Env::new("fake-s");
    let open = env.start();
    env.say("keep me");
    let closed = env.start();
    env.say("old one");
    let journals = env.dir.path().join("loa/sessions");
    assert!(journals.join(open.as_str()).exists());

    // The active session, from its sidebar row.
    env.send(Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::Delete(closed.to_string()))));
    assert!(active(&env.app).is_none());
    assert!(env.app.ctx.shell.sidebar().session(closed.as_str()).is_none());
    assert!(!journals.join(closed.as_str()).exists());

    // A session that isn't open, after a restart.
    drop(env.app);
    let mut app = build(env.dir.path(), "fake-s", &env.seen, &env.reply);
    assert!(app.ctx.shell.sidebar().session(open.as_str()).is_some(), "listed in Recents");
    update(&mut app, Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::Delete(open.to_string()))));
    assert!(app.ctx.shell.sidebar().session(open.as_str()).is_none());
    assert!(!journals.join(open.as_str()).exists());
}

#[test]
fn forking_from_the_palette_starts_a_linked_session_with_every_contribution() {
    let mut env = Env::new("fake-s");
    let rules = env.home().join(".omp/agent/rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(rules.join("tabs.md"), "Indent with tabs.").unwrap();
    let parent = env.start();
    env.say("the code word is HERON");

    env.command("sessions.fork");
    let fork = active(&env.app).expect("the fork is active").to_owned();
    assert_ne!(fork, parent.as_str());
    let dom = env.active_dom();
    assert_eq!(root_attr(&dom, attr::PARENT_SESSION).as_deref(), Some(parent.as_str()));
    assert_eq!(root_attr(&dom, attr::TITLE).as_deref(), Some("Fork of the code word is HERON"));
    let config = env.last_config();
    assert_eq!(config.session_id, fork);
    let context = config.context.unwrap_or_default();
    assert!(context.contains("HERON"), "the history is carried: {context}");
    assert!(context.contains("Indent with tabs."), "the areas' contributions are in: {context}");
    assert_eq!(env.app.ctx.shell.sidebar().session(&fork).map(|s| s.title.as_str()), Some("Fork of the code word is HERON"));

    // The Session tree shows both, the fork under its parent's turn.
    let rows: Vec<String> = env.app.areas.sessions.tree.view.rows().map(|row| format!("{:?}", row.kind)).collect();
    assert!(rows.iter().any(|row| row.contains(parent.as_str())), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains(&fork)), "{rows:?}");

    // A fork survives a restart even before its first turn.
    drop(env.app);
    let app = build(env.dir.path(), "fake-s", &env.seen, &env.reply);
    assert!(app.ctx.shell.sidebar().session(&fork).is_some());
}

#[test]
fn clicking_a_user_message_rewinds_into_a_branch_before_it() {
    let mut env = Env::new("fake-s");
    let parent = env.start();
    env.say("first HERON");
    env.say("second OTTER");
    let dom = env.active_dom();
    let (second, _) = user_messages(&dom).into_iter().find(|(_, text)| text.contains("OTTER")).unwrap();

    env.send(Message::Chat(loa_ui_chat::Message::Rewind(second)));
    let branch = active(&env.app).unwrap().to_owned();
    assert_ne!(branch, parent.as_str());
    let context = env.last_config().context.unwrap_or_default();
    assert!(context.contains("HERON") && !context.contains("OTTER"), "{context}");
    let dom = env.active_dom();
    assert_eq!(root_attr(&dom, attr::PARENT_SESSION).as_deref(), Some(parent.as_str()));
    assert_eq!(root_attr(&dom, attr::TITLE).as_deref(), Some("first HERON (rewound)"));
    assert!(env.app.ctx.services.library.load(parent.as_str()).is_ok(), "the old branch is kept");

    // Back on the parent, the tree lists its turns and the branch.
    env.send(Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::OpenSession(parent.to_string()))));
    let tree = &env.app.areas.sessions.tree.view;
    let rows: Vec<String> = tree.rows().map(|row| format!("{:?}", row.kind)).collect();
    assert!(rows.iter().any(|row| row.contains(&branch)), "{rows:?}");
    let first_turn = tree
        .rows()
        .find(|row| matches!(row.kind, session_tree::RowKind::Turn { first_line, .. } if first_line == "first HERON"))
        .expect("the parent's turns are listed")
        .index;
    env.send(Message::Sessions(SessionsMessage::Tree(session_tree::Message::Fork(first_turn))));
    let fork = active(&env.app).unwrap().to_owned();
    assert!(fork != branch && fork != parent.as_str());
    let context = env.last_config().context.unwrap_or_default();
    assert!(context.contains("HERON") && !context.contains("OTTER"), "fork through the first turn: {context}");
}

#[test]
fn compact_command_summarizes_into_a_fresh_session() {
    let mut env = Env::new("fake-s");
    let model = env.small_model("SUMMARY: the user likes herons");
    let old = env.start();
    env.say("I like herons");
    env.say("/compact keep the bird names");

    let new = active(&env.app).unwrap().to_owned();
    assert_ne!(new, old.as_str());
    assert!(env.app.ctx.sessions.get(old.as_str()).is_none(), "the compacted session is closed");
    let context = env.last_config().context.unwrap_or_default();
    assert!(context.contains("SUMMARY: the user likes herons"), "{context}");
    let asked = model.prompts.lock().unwrap().join("\n");
    assert!(asked.contains("I like herons") && asked.contains("keep the bird names"), "{asked}");
    let old_dom = env.app.ctx.services.library.load(old.as_str()).unwrap();
    let compaction = old_dom.iter().find(|node| node.kind == ElementKind::Compaction).expect("a compaction in the old session");
    assert_eq!(compaction.text, "SUMMARY: the user likes herons");
    assert!(!env.seen.lock().unwrap().prompts.iter().any(|prompt| prompt.contains("/compact")), "not sent to the harness");
    assert_eq!(env.app.ctx.shell.sidebar().session(&new).map(|s| s.title.as_str()), Some("I like herons (compacted)"));
    assert!(!env.app.ctx.composer.is_busy(), "a command isn't a turn: the composer is ready");

    // The palette entry too.
    let _ = env.app.update(Message::Palette(palette::Message::Open));
    let titles: Vec<String> = env.app.palette.commands().iter().map(|c| c.title.clone()).collect();
    assert!(titles.iter().any(|t| t == "Compact session"), "{titles:?}");
}

#[test]
fn a_model_role_from_the_palette_switches_the_running_session() {
    let mut env = Env::new("fake-s");
    let config = env.home().join(".omp/agent/config.yml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "modelRoles:\n  smol: gone/model, test/fast\n").unwrap();
    env.start();
    let _ = env.app.update(Message::Palette(palette::Message::Open));
    let titles: Vec<String> = env.app.palette.commands().iter().map(|c| c.title.clone()).collect();
    assert!(titles.iter().any(|t| t == "Model role: smol (gone/model)"), "{titles:?}");

    env.command("sessions.role.smol");
    let dom = env.active_dom();
    assert_eq!(root_attr(&dom, attr::MODEL).as_deref(), Some("test/fast"), "the first available candidate");
    assert_eq!(root_attr(&dom, attr::MODEL_ROLE).as_deref(), Some("smol"));
    assert_eq!(env.seen.lock().unwrap().models, ["test/fast"]);
}

#[test]
fn the_agents_tab_lists_jobs_live_and_opens_a_subagent_as_a_linked_child() {
    let mut env = Env::new("omp");
    let parent = env.start();
    env.say("run a subagent");
    let native = env.dir.path().join("subagent.jsonl");
    std::fs::write(&native, "{\"type\":\"session\"}\nlook around\n").unwrap();
    let writer = Arc::clone(env.app.ctx.sessions.active().unwrap().writer());
    let dom = env.active_dom();
    let turn = crate::session::dom_turns(&dom).last().unwrap().id;
    let attrs: [(&str, Value); 4] = [
        ("kind", "subagent".into()),
        ("status", "running".into()),
        ("session", native.display().to_string().into()),
        ("started", Value::Int(now_ms() - 5_000)),
    ];
    let job = writer.commit(Draft::insert(turn, ElementKind::Job, attrs, "")).unwrap().node;
    let wait: [(&str, Value); 2] = [("tool", "wait".into()), ("status", "running".into())];
    writer.commit(Draft::insert(turn, ElementKind::Call, wait, "")).unwrap();
    env.pump();

    let agents = &env.app.areas.sessions.agents;
    assert_eq!(agents.jobs().count(), 1, "listed live");
    assert!(agents.is_awaited(job), "omp's wait is blocked on it");
    let task = env.app.command(&palette::CommandId::new("tab.agents"));
    run(&mut env.app, task);
    assert_eq!(env.app.ctx.shell.active_tab(), Some("agents"));

    env.send(Message::Sessions(SessionsMessage::Agents(agents::Message::Open(job))));
    let child = active(&env.app).expect("the child is open").to_owned();
    assert_ne!(child, parent.as_str());
    let dom = env.active_dom();
    assert_eq!(root_attr(&dom, attr::PARENT_SESSION).as_deref(), Some(parent.as_str()));
    assert!(user_messages(&dom).iter().any(|(_, text)| text == "look around"));
}

#[test]
fn plan_mode_blocks_changing_tools_and_raises_a_plan_review() {
    let mut env = Env::new("omp");
    let id = env.start();
    env.send(Message::Composer(composer::Message::ModePicked(SessionMode::Plan)));
    assert_eq!(root_attr(&env.active_dom(), attr::MODE).as_deref(), Some("plan"));

    // A tool that changes files is denied without asking the user; the
    // agent's answer then goes up for review as the plan.
    *env.reply.lock().unwrap() = "1. Write a.txt\n2. Run the tests".to_owned();
    env.say("ask to write a.txt");
    let dom = env.active_dom();
    let approval = dom
        .iter()
        .find(|node| node.kind == ElementKind::Decision && node.str_attr(attr::KIND) == Some("approval"))
        .expect("an approval");
    assert_eq!(approval.attr("answer"), Some(&Value::List(vec![Value::from(answer::DENY)])));
    let first = env.seen.lock().unwrap().prompts[0].clone();
    assert!(first.starts_with("ask to write a.txt") && first.ends_with(loa_features::plan::PLAN_STEP), "{first}");
    let review = dom
        .iter()
        .find(|node| loa_features::plan::is_app_plan(&dom, node.id))
        .expect("a plan review under the session");
    assert_eq!(review.text, "1. Write a.txt\n2. Run the tests");
    let review = review.id;
    assert!(env.app.areas.decisions.card(id.as_str(), review).is_some(), "drawn as a decision card");

    *env.reply.lock().unwrap() = "done".to_owned();
    let approve = Answer { values: vec![answer::APPROVE.to_owned()], reason: None, by: String::new() };
    env.send(Message::Sessions(SessionsMessage::AnswerPlan { session: id.clone(), decision: review, answer: approve }));
    let dom = env.active_dom();
    assert_eq!(root_attr(&dom, attr::MODE).as_deref(), Some("normal"));
    let prompts = env.seen.lock().unwrap().prompts.clone();
    assert!(prompts.last().unwrap().starts_with("The user approved this plan"), "{prompts:?}");
    assert!(!prompts.last().unwrap().contains(loa_features::plan::PLAN_STEP), "plan mode is over");
}

#[test]
fn rules_go_into_the_context_and_advisor_notes_into_the_next_prompt() {
    let mut env = Env::new("fake-s");
    let rules = env.home().join(".omp/agent/rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(rules.join("tests.md"), "Run the tests after every change.").unwrap();
    let model = env.small_model("You changed code without running the tests.");
    env.start();
    let context = env.last_config().context.unwrap_or_default();
    assert!(context.contains("Run the tests after every change."), "{context}");

    env.say("edit main.rs");
    let asked = model.prompts.lock().unwrap().join("\n");
    assert!(asked.contains("edit main.rs") && asked.contains("Run the tests"), "{asked}");
    let dom = env.active_dom();
    let note = dom.iter().find(|node| node.str_attr(attr::BY) == Some("advisor")).expect("the note is recorded in the turn");
    assert_eq!(note.text, "You changed code without running the tests.");

    std::fs::write(rules.join("tests.md"), "Run the tests twice.").unwrap();
    env.say("next");
    let sent = env.seen.lock().unwrap().prompts.last().cloned().unwrap();
    assert!(sent.contains("[Advisor note on your last turn]\nYou changed code without running the tests."), "{sent}");
    assert!(sent.contains("Run the tests twice."), "changed rules are sent: {sent}");
    assert!(sent.ends_with("next"));
}

#[test]
fn the_import_screen_imports_native_sessions_and_exports_native_and_xml() {
    let mut env = Env::new("omp");
    let native = env.home().join(".omp/agent/sessions/-proj/2026_a.jsonl");
    std::fs::create_dir_all(native.parent().unwrap()).unwrap();
    std::fs::write(&native, "{\"type\":\"title\",\"title\":\"Old work\"}\nfix the parser\n").unwrap();

    env.command("sessions.import");
    assert_eq!(env.app.ctx.screen(), Some(crate::area::AreaId::Sessions));
    assert_eq!(env.app.areas.sessions.transfer.found.len(), 1);
    assert_eq!(env.app.areas.sessions.transfer.found[0].title.as_deref(), Some("Old work"));

    env.send(Message::Sessions(SessionsMessage::Transfer(super::transfer::Message::Import(native.clone()))));
    let imported = active(&env.app).expect("the import is open").to_owned();
    assert!(env.app.ctx.screen().is_none());
    let dom = env.active_dom();
    assert_eq!(root_attr(&dom, attr::NATIVE_ID).as_deref(), Some("native-imported"));
    assert!(user_messages(&dom).iter().any(|(_, text)| text == "fix the parser"));
    assert_eq!(env.app.ctx.shell.sidebar().session(&imported).map(|s| s.title.as_str()), Some("fix the parser"));

    // A file picked by path.
    let picked = env.dir.path().join("picked.jsonl");
    std::fs::write(&picked, "{\"type\":\"session\",\"version\":3}\nfrom a path\n").unwrap();
    env.command("sessions.import");
    env.send(Message::Sessions(SessionsMessage::Transfer(super::transfer::Message::Path(picked.display().to_string()))));
    env.send(Message::Sessions(SessionsMessage::Transfer(super::transfer::Message::ImportPath)));
    assert!(user_messages(&env.active_dom()).iter().any(|(_, text)| text == "from a path"));

    let id = active(&env.app).unwrap().to_owned();
    env.command("sessions.export-xml");
    let xml = std::fs::read_to_string(env.dir.path().join(format!("loa/exports/{id}.xml"))).unwrap();
    assert!(xml.contains("from a path"), "{xml}");
    env.command("sessions.export");
    let jsonl = std::fs::read_to_string(env.dir.path().join(format!("loa/exports/{id}.jsonl"))).unwrap();
    assert!(jsonl.contains("from a path"));
    assert!(env.app.ctx.notice().is_some_and(|notice| notice.starts_with("Exported to")));

    // Unknown files are refused.
    let unknown = env.dir.path().join("notes.txt");
    std::fs::write(&unknown, "hello").unwrap();
    env.send(Message::Sessions(SessionsMessage::Transfer(super::transfer::Message::Import(unknown))));
    assert!(env.app.ctx.notice().is_some_and(|notice| notice.starts_with("Can't import")));
}

#[test]
fn a_spawned_session_that_fails_to_start_leaves_nothing_behind() {
    let mut env = Env::new("fake-s");
    env.start();
    let id = crate::session::new_session_id();
    let library = Arc::clone(&env.app.ctx.services.library);
    let created = id.clone();
    let start: crate::area::SpawnStart = Arc::new(move |_| {
        let (library, created) = (Arc::clone(&library), created.clone());
        Box::pin(async move {
            library.create(&created).map_err(|error| error.to_string())?;
            Err("boom".to_owned())
        })
    });
    let spawn = crate::area::Spawn { id: SessionId::from(id.clone()), harness: "fake-s".into(), start, prompt: None };
    env.app.ctx.request(crate::area::Request::Spawn(spawn));
    update(&mut env.app, Message::Sessions(SessionsMessage::Done(Ok(()))));
    assert!(env.app.ctx.sessions.starting().is_none());
    assert_eq!(env.app.ctx.notice(), Some("boom"));
    let dir = env.dir.path().join("loa/sessions").join(&id);
    assert!(!dir.exists(), "the half created session is deleted; deleting again: {:?}", env.app.ctx.services.library.delete(&id));
}

/// The app on the real harness adapters, with `home` under `dir`.
fn build_native(dir: &Path) -> App {
    let node = Node::new(NodeIdentity::from_secret_bytes(&[9; 32]), "testhost".into());
    let dirs = Dirs::new(dir.join("loa"));
    let harnesses = Harnesses::detect(&node.key, &dirs.omp);
    let mut app = App::build(Services::new(node, dirs, harnesses, dir.to_owned()).unwrap()).0;
    app.areas.sessions.set_home(dir.join("home"));
    app
}

fn json_lines(text: &str) -> Vec<serde_json::Value> {
    text.lines().filter(|line| !line.trim().is_empty()).map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// Imports `native` from the import screen (from its discovered list when
/// `listed`, else by path), checks the session opened with `first_user`,
/// exports it from the same screen and returns the exported text.
fn import_and_export(app: &mut App, dir: &Path, native: &Path, listed: bool, first_user: &str) -> String {
    let transfer = |message| Message::Sessions(SessionsMessage::Transfer(message));
    let task = app.command(&palette::CommandId::new("sessions.import"));
    run(app, task);
    if listed {
        assert!(app.areas.sessions.transfer.found.iter().any(|found| found.path == native), "{} is listed", native.display());
        update(app, transfer(super::transfer::Message::Import(native.to_owned())));
    } else {
        update(app, transfer(super::transfer::Message::Path(native.display().to_string())));
        update(app, transfer(super::transfer::Message::ImportPath));
    }
    let id = active(app).unwrap_or_else(|| panic!("{} opened: {:?}", native.display(), app.ctx.notice())).to_owned();
    let dom = app.ctx.services.library.load(&id).unwrap();
    assert!(user_messages(&dom).iter().any(|(_, text)| text.contains(first_user)), "{} imported", native.display());

    let out = dir.join("out").join(native.file_name().unwrap());
    let task = app.command(&palette::CommandId::new("sessions.import"));
    run(app, task);
    update(app, transfer(super::transfer::Message::ExportPath(out.display().to_string())));
    update(app, transfer(super::transfer::Message::Export { xml: false }));
    assert_eq!(app.ctx.notice().map(str::to_owned), Some(format!("Exported to {}", out.display())));
    std::fs::read_to_string(&out).unwrap()
}

#[test]
fn native_sessions_of_every_harness_round_trip_through_the_import_screen() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let mut app = build_native(dir.path());
    let write = |path: &Path, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };

    // omp JSONL (#27).
    let omp = home.join(".omp/agent/sessions/-work/2026-02-16_session-1.jsonl");
    let omp_file = [
        r#"{"type":"session","version":3,"id":"session-1","timestamp":"2026-02-16T10:20:30.000Z","cwd":"/work"}"#,
        r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"hello omp"}]},"id":"entry-0","parentId":null,"timestamp":"2026-02-16T10:20:31.000Z"}"#,
        r#"{"type":"message","message":{"role":"assistant","provider":"acme","model":"large","content":[{"type":"text","text":"answer"}],"stopReason":"stop"},"id":"entry-1","parentId":"entry-0","timestamp":"2026-02-16T10:20:32.000Z"}"#,
    ]
    .join("\n")
        + "\n";
    write(&omp, &omp_file);

    // Claude Code transcript (#37).
    let claude = home.join(".claude/projects/-invented-project/11111111-2222-4333-8444-555555555555.jsonl");
    let envelope = |kind: &str, uuid: &str, parent: Option<&str>| {
        serde_json::json!({
            "type": kind, "uuid": uuid, "parentUuid": parent,
            "sessionId": "11111111-2222-4333-8444-555555555555",
            "timestamp": "2026-01-02T03:04:05.000Z", "cwd": "/invented/project",
            "isSidechain": false, "version": "2.1.0", "futureEnvelope": {"nested": [1, false]}
        })
    };
    let mut user = envelope("user", "u1", None);
    user["message"] = serde_json::json!({"role": "user", "content": [{"type": "text", "text": "Investigate the claude bug"}]});
    let mut assistant = envelope("assistant", "a1", Some("u1"));
    assistant["message"] = serde_json::json!({"id": "msg_one", "role": "assistant", "model": "claude-fiction", "content": [{"type": "text", "text": "Checking file"}]});
    let claude_lines = [user, assistant, serde_json::json!({"type": "summary", "summary": "Invented title", "leafUuid": "a1"})];
    write(&claude, &(claude_lines.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n") + "\n"));

    // Codex rollout (#40).
    let codex = home.join(".codex/sessions/2026/02/10/rollout-2026-02-10T09-00-00-0000aaaa.jsonl");
    let codex_file = include_str!("../../../../loa-adapter-codex/tests/fixtures/rollout-basic.jsonl");
    write(&codex, codex_file);

    // An OpenCode export, picked by path (#43).
    let opencode = dir.path().join("picked/ses_fixture.json");
    let opencode_file = serde_json::json!({
        "info": {"id": "ses_fixture", "title": "Example", "directory": "/work", "time": {"created": 100, "updated": 130}, "projectID": "p", "version": "1"},
        "messages": [
            {"info": {"id": "msg_user", "sessionID": "ses_fixture", "role": "user", "time": {"created": 101}, "model": {"providerID": "p", "modelID": "m"}},
             "parts": [{"id": "prt_user", "sessionID": "ses_fixture", "messageID": "msg_user", "type": "text", "text": "hi opencode"}]},
            {"info": {"id": "msg_assistant", "sessionID": "ses_fixture", "role": "assistant", "parentID": "msg_user", "time": {"created": 102, "completed": 130}, "providerID": "p", "modelID": "m", "tokens": {"input": 2, "output": 4}},
             "parts": [{"id": "prt_answer", "sessionID": "ses_fixture", "messageID": "msg_assistant", "type": "text", "text": "done", "time": {"start": 110, "end": 130}}]}
        ]
    });
    write(&opencode, &opencode_file.to_string());

    let exported = import_and_export(&mut app, dir.path(), &omp, true, "hello omp");
    assert_eq!(json_lines(&exported), json_lines(&omp_file), "omp round trip");
    let exported = import_and_export(&mut app, dir.path(), &claude, true, "Investigate the claude bug");
    assert_eq!(json_lines(&exported), claude_lines.to_vec(), "Claude Code round trip");
    let exported = import_and_export(&mut app, dir.path(), &codex, true, "List the seed packets");
    assert_eq!(json_lines(&exported), json_lines(codex_file), "Codex round trip");
    let exported = import_and_export(&mut app, dir.path(), &opencode, false, "hi opencode");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&exported).unwrap(), opencode_file, "OpenCode round trip");
}

/// The names the composer's `/` popup offers.
fn slash_names(app: &App) -> Vec<String> {
    app.ctx.composer.commands().iter().map(|command| command.name.clone()).collect()
}

#[test]
fn slash_commands_run_through_their_areas_and_the_harness_lists_its_own() {
    let mut env = Env::new("fake-s");
    let config = env.home().join(".omp/agent/config.yml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "modelRoles:\n  smol: test/small\n").unwrap();
    let first = env.start();
    env.say("the code word is HERON");
    let names = slash_names(&env.app);
    for name in ["new", "model", "role", "compact", "fork", "plan", "normal", "export", "export-xml", "import"] {
        assert!(names.iter().any(|known| known == name), "/{name} is offered: {names:?}");
    }

    // The harness's own commands, as its adapter records them, join the
    // list; one named like an app command doesn't.
    let writer = Arc::clone(env.app.ctx.sessions.active().unwrap().writer());
    let root = env.active_dom().root().unwrap().id;
    let listed = r#"{"type":"available_commands_update","commands":[{"name":"dirs","description":"List this session's workspace directories"},{"name":"compact","description":"Compact the conversation"}]}"#;
    let attrs: [(&str, Value); 2] = [("harness", "omp".into()), ("type", loa_ui_chat::COMMANDS_EXT.into())];
    writer.commit(Draft::insert(root, ElementKind::Ext, attrs, listed)).unwrap();
    env.pump();
    let commands = env.app.ctx.composer.commands().to_vec();
    let dirs = commands.iter().find(|command| command.name == "dirs").expect("the harness's command");
    assert_eq!(dirs.description, "omp: List this session's workspace directories");
    assert_eq!(commands.iter().filter(|command| command.name == "compact").count(), 1);
    assert!(!commands.iter().any(|command| command.description.starts_with("omp: Compact")), "the app's /compact runs");

    // A harness command goes to the harness as typed.
    env.say("/dirs");
    assert_eq!(env.seen.lock().unwrap().prompts.last().map(String::as_str), Some("/dirs"));

    // The app's commands run in their areas, never reaching the harness.
    env.say("/model test/fast");
    assert_eq!(env.seen.lock().unwrap().models, ["test/fast"], "notice: {:?}", env.app.ctx.notice());
    env.say("/role smol");
    assert_eq!(root_attr(&env.active_dom(), attr::MODEL).as_deref(), Some("test/small"));
    assert_eq!(root_attr(&env.active_dom(), attr::MODEL_ROLE).as_deref(), Some("smol"));
    env.say("/role nope");
    assert_eq!(env.app.ctx.notice(), Some("omp has no model role nope; its roles: smol"));
    // This harness can't be held to read-only tools; the area says so.
    // (Plan mode itself is `plan_mode_blocks_changing_tools_and_raises_a_plan_review`.)
    env.say("/plan");
    assert_eq!(
        env.app.ctx.notice(),
        Some("Can't enter plan mode on fake-s: feature port failed: this harness can't be limited to read-only tools")
    );
    env.say("/normal");
    assert!(!env.app.areas.sessions.plans.contains_key(first.as_str()), "in normal mode");
    env.say("/export-xml");
    let exported = env.dir.path().join(format!("loa/exports/{first}.xml"));
    assert!(exported.is_file(), "notice: {:?}", env.app.ctx.notice());
    env.say("/import");
    assert_eq!(env.app.ctx.screen(), Some(crate::area::AreaId::Sessions));
    env.app.ctx.close_screen();
    env.say("/fork");
    let fork = active(&env.app).unwrap().to_owned();
    assert_ne!(fork, first.as_str());
    assert_eq!(root_attr(&env.active_dom(), attr::PARENT_SESSION).as_deref(), Some(first.as_str()));
    env.say("/new");
    let new = active(&env.app).unwrap().to_owned();
    assert!(new != fork && new != first.as_str());
    let prompts = env.seen.lock().unwrap().prompts.clone();
    assert!(!prompts.iter().any(|prompt| ["/model", "/role", "/plan", "/normal", "/export", "/import", "/fork", "/new"]
        .iter()
        .any(|command| prompt.starts_with(command))), "{prompts:?}");
    assert!(!env.app.ctx.composer.is_busy(), "commands aren't turns");
    assert_eq!(slash_names(&env.app).iter().filter(|name| *name == "dirs").count(), 0, "a new session has no harness list yet");
}

#[test]
fn slash_plan_and_normal_switch_the_mode() {
    let mut env = Env::new("omp");
    let id = env.start();
    env.say("/plan");
    assert!(env.app.areas.sessions.plans.contains_key(id.as_str()), "notice: {:?}", env.app.ctx.notice());
    assert_eq!(root_attr(&env.active_dom(), attr::MODE).as_deref(), Some("plan"));
    env.say("/normal");
    assert!(!env.app.areas.sessions.plans.contains_key(id.as_str()));
    assert_eq!(root_attr(&env.active_dom(), attr::MODE).as_deref(), Some("normal"));
    assert!(env.seen.lock().unwrap().prompts.is_empty(), "not sent to the harness");
}

#[test]
fn a_job_or_a_branch_shows_its_tab_without_taking_the_panel() {
    let mut env = Env::new("omp");
    env.start();
    env.say("run a subagent");
    env.app.ctx.shell.show_tab("diff");
    let listed = |app: &App, id: &str| app.ctx.shell.tabs().iter().any(|tab| tab.id == id);
    assert!(!listed(&env.app, "agents") && !listed(&env.app, "session"));

    // A job starting lists the Agents tab; the user keeps reading Diff.
    let writer = Arc::clone(env.app.ctx.sessions.active().unwrap().writer());
    let turn = crate::session::dom_turns(&env.active_dom()).last().unwrap().id;
    let job: [(&str, Value); 3] = [("kind", "subagent".into()), ("status", "running".into()), ("started", Value::Int(now_ms()))];
    writer.commit(Draft::insert(turn, ElementKind::Job, job.clone(), "")).unwrap();
    env.pump();
    assert!(listed(&env.app, "agents"));
    assert_eq!(env.app.ctx.shell.active_tab(), Some("diff"));
    assert!(env.app.ctx.shell.panel_open());

    // With the panel closed, the next job opens it on the Agents tab.
    env.app.ctx.shell.set_panel_open(false);
    writer.commit(Draft::insert(turn, ElementKind::Job, job, "")).unwrap();
    env.pump();
    assert!(env.app.ctx.shell.panel_open());
    assert_eq!(env.app.ctx.shell.active_tab(), Some("agents"));

    // A fork lists the Session tree once it runs.
    env.app.ctx.shell.show_tab("diff");
    env.command("sessions.fork");
    assert!(listed(&env.app, "session"));
    assert_eq!(env.app.ctx.shell.active_tab(), Some("diff"));
}

/// Records the URLs it was asked to open.
#[derive(Default)]
struct Browser(Mutex<Vec<String>>);

impl loa_models::login::UrlOpener for Browser {
    fn open_url(&self, url: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(url.to_owned());
        Ok(())
    }
}

#[test]
fn web_links_in_the_transcript_open_in_the_browser() {
    let mut env = Env::new("fake-s");
    let browser = Arc::new(Browser::default());
    env.app.areas.sessions.set_opener(Arc::clone(&browser) as _);
    env.start();
    env.send(Message::Chat(loa_ui_chat::Message::Link("https://example.com/docs".into())));
    env.send(Message::Chat(loa_ui_chat::Message::Link("file:///etc/passwd".into())));
    assert_eq!(*browser.0.lock().unwrap(), ["https://example.com/docs"]);
    assert_eq!(env.app.ctx.notice(), Some("Only web links open in the browser: file:///etc/passwd"));
}

#[test]
fn deleting_a_pinned_session_drops_its_pin() {
    let mut env = Env::new("fake-s");
    let id = env.start();
    env.say("pin me");
    env.send(Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::TogglePin(id.to_string()))));
    let saved = |env: &Env| env.app.ctx.services.config.load().unwrap().config.pinned;
    assert!(saved(&env).contains(id.as_str()));

    env.send(Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::Delete(id.to_string()))));
    assert!(!saved(&env).contains(id.as_str()), "the saved pin is gone");
    assert!(!env.app.ctx.shell.sidebar().pins().contains(id.as_str()));
}
