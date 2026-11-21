//! The workflows area driven through `App::update`: omp's agent directory is
//! fixed and its settings validator accepts everything, so nothing here
//! starts omp. Sessions run on a fake omp harness that records what it was
//! told, in order.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use loa_adapter::{
    Adapter, AdapterError, Answer, Capabilities, HarnessSession, ModelSelection, ModelSwitch, Prompt, Registry, SessionConfig, SessionState,
};
use loa_bus::Policy;
use loa_core::{Dom, Draft, ElementKind, NodeId, PatchSink, Role, TurnStatus, Value, attr, now_ms};
use loa_identity::NodeIdentity;
use loa_models::agents::{AgentDefinition, Overwrite};
use loa_ui_composer as composer;
use loa_ui_palette as palette;
use loa_ui_workflows::Access;

use super::*;
use crate::app::tests::{fake_app, prompt, run, update};
use crate::app::{App, Message as AppMessage};
use crate::services::{Dirs, Harnesses, Node, Services};
use crate::session::Feed;

struct Accept;

#[async_trait]
impl SettingsValidator for Accept {
    async fn check_config(&self, _: &str, _: &[(String, serde_json::Value)]) -> loa_models::Result<()> {
        Ok(())
    }

    async fn check_models(&self, _: &str) -> loa_models::Result<()> {
        Ok(())
    }
}

/// What the model is told with the next prompt after a switch to DEEP.
const NOTE_DEEP: &str = "The active model role is now `DEEP`.\n\n";

const CONFIG: &str = "modelRoles:\n  default: test/fast\n  DEEP: test/deep\n";
const RULES: &str = "# Rules\n\nBe kind.\n\n## Deep Mode\n\nWhen the active model role is `DEEP`, operate in Deep Mode.\n";

/// Fills omp's agent directory: config, hand-written rules and agents a, b
/// and c (disabled).
fn agent_dir(dir: &Path) -> PathBuf {
    let agent_dir = dir.join("home").join(".omp").join("agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("config.yml"), CONFIG).unwrap();
    std::fs::write(agent_dir.join("RULES.md"), RULES).unwrap();
    for name in ["a", "b", "c"] {
        let definition = AgentDefinition::new(name, "An agent").unwrap();
        let path = loa_models::agents::write(&agent_dir.join("agents"), &definition, Overwrite::No).unwrap();
        if name == "c" {
            loa_models::agents::set_enabled(&path, false).unwrap();
        }
    }
    agent_dir
}

fn fix(app: &mut App, agent_dir: &Path) {
    app.areas.workflows.use_fixed(Fixed { agent_dir: agent_dir.to_owned(), validator: Arc::new(Accept) });
    app.ctx.services.omp_agent_dir.set(agent_dir.to_owned());
    app.areas.sessions.set_home(agent_dir.parent().unwrap().parent().unwrap().to_owned());
}

fn fill(app: &mut App, message: screen::Message) {
    update(app, AppMessage::Workflows(Message::Screen(message)));
}

fn command(app: &mut App, id: &str) {
    let task = app.command(&palette::CommandId::new(id));
    run(app, task);
}

fn palette_command(app: &mut App, title: &str) -> Option<palette::Command> {
    update(app, AppMessage::Palette(palette::Message::Open));
    app.palette.commands().iter().find(|command| command.title == title).cloned()
}

/// Creates "Research", allowing a and b and disallowing c, through the screen.
fn create_research(app: &mut App) {
    update(app, AppMessage::Workflows(Message::Open));
    fill(app, screen::Message::Name("Research".into()));
    fill(app, screen::Message::Access("a".into(), Access::Allow));
    fill(app, screen::Message::Access("b".into(), Access::Allow));
    fill(app, screen::Message::Access("c".into(), Access::Disallow));
    fill(app, screen::Message::Save);
}

#[derive(Default)]
struct Log {
    /// `prompt:<text>` and `model:<id>`, in the order the harness got them.
    events: Vec<String>,
    configs: Vec<SessionConfig>,
    /// Turns left running by a `hold` prompt.
    held: Vec<(Arc<dyn PatchSink>, NodeId)>,
}

type Shared = Arc<Mutex<Log>>;

/// A harness that answers every prompt at once, except one starting with
/// `hold`, whose turn runs until the test finishes it.
struct TestAdapter {
    log: Shared,
}

#[async_trait]
impl Adapter for TestAdapter {
    fn harness(&self) -> &'static str {
        "omp"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { model_switch: ModelSwitch::PerCall, ..Capabilities::default() }
    }

    async fn start(&self, config: SessionConfig, sink: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        let mut attrs: Vec<(&str, Value)> = vec![
            ("id", config.session_id.as_str().into()),
            ("node", config.node.as_str().into()),
            ("harness", "omp".into()),
            ("created", Value::Int(now_ms())),
        ];
        if let Some(model) = &config.model {
            attrs.push((attr::MODEL, model.as_str().into()));
        }
        if let Some(role) = &config.model_role {
            attrs.push((attr::MODEL_ROLE, role.as_str().into()));
        }
        let root = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""))?.node;
        self.log.lock().unwrap().configs.push(config);
        Ok(Box::new(TestSession { sink, root, log: Arc::clone(&self.log) }))
    }

    async fn resume(&self, _: SessionConfig, _: &Dom, _: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
        Err(AdapterError::Unsupported { harness: "omp", what: "resume" })
    }
}

struct TestSession {
    sink: Arc<dyn PatchSink>,
    root: NodeId,
    log: Shared,
}

fn finish_turn(sink: &dyn PatchSink, turn: NodeId) -> loa_adapter::Result<()> {
    let reply: [(&str, Value); 2] = [("role", Role::Assistant.into()), ("status", "done".into())];
    sink.commit(Draft::insert(turn, ElementKind::Message, reply, "ok"))?;
    sink.commit(Draft::set(turn, "status", TurnStatus::Done))?;
    Ok(())
}

#[async_trait]
impl HarnessSession for TestSession {
    async fn send(&self, prompt: Prompt) -> loa_adapter::Result<()> {
        self.log.lock().unwrap().events.push(format!("prompt:{}", prompt.text));
        let turn = self.sink.commit(Draft::insert(self.root, ElementKind::Turn, [("status", Value::from(TurnStatus::Running))], ""))?.node;
        self.sink.commit(Draft::insert(turn, ElementKind::Message, [("role", Value::from(Role::User))], prompt.text.as_str()))?;
        if prompt.text.starts_with("hold") {
            self.log.lock().unwrap().held.push((Arc::clone(&self.sink), turn));
            return Ok(());
        }
        finish_turn(self.sink.as_ref(), turn)
    }

    async fn interrupt(&self) -> loa_adapter::Result<()> {
        Ok(())
    }

    async fn answer(&self, decision: NodeId, _: Answer) -> loa_adapter::Result<()> {
        Err(AdapterError::UnknownDecision(decision))
    }

    async fn set_model(&self, selection: ModelSelection) -> loa_adapter::Result<()> {
        self.log.lock().unwrap().events.push(format!("model:{}", selection.model));
        Ok(())
    }

    fn harness(&self) -> &'static str {
        "omp"
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

struct Env {
    _dir: tempfile::TempDir,
    app: App,
    log: Shared,
    agent_dir: PathBuf,
    feeds: Vec<(loa_bus::SessionId, loa_bus::Subscription)>,
}

impl Env {
    /// The app on a fake omp harness, with omp's agent directory filled.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let log = Shared::default();
        let mut harnesses = Harnesses { registry: Registry::new(), list: Vec::new() };
        harnesses.add(Arc::new(TestAdapter { log: Arc::clone(&log) }), "omp", true, None, None);
        let node = Node::new(NodeIdentity::from_secret_bytes(&[7; 32]), "testhost".into());
        let services = Services::new(node, Dirs::new(dir.path().join("loa")), harnesses, dir.path().to_owned()).unwrap();
        let mut app = App::build(services).0;
        crate::app::tests::list_recents(&mut app);
        let agent_dir = agent_dir(dir.path());
        fix(&mut app, &agent_dir);
        Self { _dir: dir, app, log, agent_dir, feeds: Vec::new() }
    }

    /// Delivers every open session's patches until nothing is left.
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
                update(&mut self.app, AppMessage::Session(id, Feed::Synced(Arc::new(dom))));
            }
            let mut idle = true;
            for index in 0..self.feeds.len() {
                while let Ok(event) = self.feeds[index].1.try_recv() {
                    idle = false;
                    let id = self.feeds[index].0.clone();
                    update(&mut self.app, AppMessage::Session(id, Feed::Events(vec![event])));
                }
            }
            if idle {
                return;
            }
        }
    }

    fn send(&mut self, message: AppMessage) {
        update(&mut self.app, message);
        self.pump();
    }

    fn say(&mut self, text: &str) {
        prompt(&mut self.app, text);
        self.pump();
    }

    fn start(&mut self) {
        self.send(AppMessage::NewSession);
        assert!(self.app.ctx.sessions.active().is_some_and(|session| session.harness().is_some()), "a running session");
    }

    /// Picks `role` in the composer's workflow picker.
    fn pick(&mut self, role: &str) {
        self.send(AppMessage::Composer(composer::Message::WorkflowPicked(role.to_owned())));
    }

    /// Ends the turns a `hold` prompt left running.
    fn release(&mut self) {
        let held = std::mem::take(&mut self.log.lock().unwrap().held);
        for (sink, turn) in held {
            finish_turn(sink.as_ref(), turn).unwrap();
        }
        self.pump();
    }

    fn events(&self) -> Vec<String> {
        self.log.lock().unwrap().events.clone()
    }

    fn role(&self) -> Option<String> {
        let dom = self.app.ctx.sessions.active().unwrap().writer().with_dom(Dom::clone);
        dom.root().and_then(|root| root.str_attr(attr::MODEL_ROLE)).map(str::to_owned)
    }

    fn picked(&self) -> Option<&str> {
        self.app.ctx.composer.workflow()
    }
}

#[test]
fn other_harnesses_cannot_open_the_editor() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    fix(&mut app, &agent_dir(dir.path()));

    update(&mut app, AppMessage::Workflows(Message::Open));
    assert_eq!(app.ctx.screen(), None);
    assert_eq!(app.ctx.notice(), Some(OMP_ONLY));

    app.ctx.clear_notice();
    let task = app.update(AppMessage::Composer(composer::Message::ManageWorkflows));
    run(&mut app, task);
    assert_eq!(app.ctx.screen(), None, "the Rules button is dimmed");
    update(&mut app, AppMessage::ComposerEvent(composer::Event::ManageWorkflows));
    assert_eq!(app.ctx.screen(), None);
    assert_eq!(app.ctx.notice(), Some(OMP_ONLY));

    assert!(!palette_command(&mut app, "New workflow").expect("listed").enabled);
    app.ctx.clear_notice();
    command(&mut app, "workflows.new");
    assert_eq!(app.ctx.screen(), None);
    assert_eq!(app.ctx.notice(), Some(OMP_ONLY));
    app.ctx.clear_notice();
    command(&mut app, "workflows.edit.DEEP");
    assert_eq!(app.ctx.screen(), None);
    assert_eq!(app.ctx.notice(), Some(OMP_ONLY));

    // The picker is dimmed for the harness.
    assert!(app.areas.workflows.picker.as_ref().is_some_and(|(_, _, enabled)| !enabled));
    update(&mut app, AppMessage::ComposerEvent(composer::Event::WorkflowChanged("DEEP".into())));
    assert_eq!(app.ctx.notice(), Some(OMP_ONLY));
}

#[test]
fn creating_a_workflow_writes_the_rules_and_the_role_and_reopens_the_same() {
    let mut env = Env::new();
    assert!(env.app.areas.workflows.picker.as_ref().is_some_and(|(_, _, enabled)| *enabled));
    assert!(palette_command(&mut env.app, "New workflow").expect("listed").enabled);

    create_research(&mut env.app);
    assert_eq!(env.app.ctx.notice(), Some("Saved workflow Research"));
    assert_eq!(env.app.ctx.screen(), Some(AreaId::Workflows));

    let rules = std::fs::read_to_string(env.agent_dir.join("RULES.md")).unwrap();
    assert!(rules.starts_with(RULES), "hand-written rules untouched: {rules}");
    assert!(rules.contains("<!-- omp-gui:workflow RESEARCH -->"), "{rules}");
    assert!(rules.contains("## Research Mode"), "{rules}");
    assert!(rules.contains("- Allowed agents: `a`, `b`."), "{rules}");
    assert!(rules.contains("- Disallowed agents: `c`."), "{rules}");
    let config = std::fs::read_to_string(env.agent_dir.join("config.yml")).unwrap();
    assert!(config.contains("RESEARCH: test/fast"), "the default role's model: {config}");

    // The picker lists the workflows, the hand-written one by its name.
    let options = env.app.areas.workflows.options();
    let listed: Vec<(&str, &str)> = options.iter().map(|option| (option.role.as_str(), option.label.as_str())).collect();
    assert_eq!(listed, [("default", "Default"), ("RESEARCH", "Research"), ("DEEP", "Deep")]);

    // Reopened from the palette it shows the same choices.
    env.app.ctx.close_screen();
    assert!(palette_command(&mut env.app, "Edit workflow: Research").expect("listed").enabled);
    command(&mut env.app, "workflows.edit.RESEARCH");
    assert_eq!(env.app.ctx.screen(), Some(AreaId::Workflows));
    let state = &env.app.areas.workflows.screen;
    assert_eq!(state.selected(), Some("RESEARCH"));
    assert_eq!(state.access("a"), Access::Allow);
    assert_eq!(state.access("b"), Access::Allow);
    assert_eq!(state.access("c"), Access::Disallow);
}

#[test]
fn a_refused_save_shows_inline_and_deleting_removes_the_workflow() {
    let mut env = Env::new();
    create_research(&mut env.app);

    // The same name again is a new workflow with a taken role.
    fill(&mut env.app, screen::Message::New);
    fill(&mut env.app, screen::Message::Name("research".into()));
    fill(&mut env.app, screen::Message::Save);
    assert_eq!(env.app.ctx.screen(), Some(AreaId::Workflows));
    assert!(env.app.areas.workflows.screen.selected().is_none(), "the draft stays");

    command(&mut env.app, "workflows.edit.RESEARCH");
    fill(&mut env.app, screen::Message::Delete);
    fill(&mut env.app, screen::Message::Delete);
    assert_eq!(env.app.ctx.notice(), Some("Deleted workflow Research"));
    let rules = std::fs::read_to_string(env.agent_dir.join("RULES.md")).unwrap();
    assert_eq!(rules, RULES);
    assert!(!std::fs::read_to_string(env.agent_dir.join("config.yml")).unwrap().contains("RESEARCH"));
    assert!(env.app.areas.workflows.options().iter().all(|option| option.role != "RESEARCH"));
}

#[test]
fn picking_a_workflow_with_no_session_sets_the_next_sessions_model_role() {
    let mut env = Env::new();
    create_research(&mut env.app);
    env.app.ctx.close_screen();

    env.pick("RESEARCH");
    assert_eq!(env.picked(), Some("RESEARCH"));
    env.start();
    {
        let log = env.log.lock().unwrap();
        let config = log.configs.last().unwrap();
        assert_eq!(config.model_role.as_deref(), Some("RESEARCH"));
        assert_eq!(config.model.as_deref(), Some("test/fast"));
    }
    assert_eq!(env.role().as_deref(), Some("RESEARCH"));
    assert_eq!(env.picked(), Some("RESEARCH"));
}

#[test]
fn picking_a_workflow_switches_the_running_session() {
    let mut env = Env::new();
    env.start();
    env.say("hello");
    assert_eq!(env.picked(), Some("default"));

    env.pick("DEEP");
    assert_eq!(env.events(), ["prompt:hello", "model:test/deep"]);
    assert_eq!(env.role().as_deref(), Some("DEEP"));
    assert_eq!(env.picked(), Some("DEEP"));
}

#[test]
fn a_pick_mid_turn_applies_after_the_turn_and_before_the_queued_prompt() {
    let mut env = Env::new();
    env.start();
    env.say("hold on");
    assert!(env.app.ctx.sessions.active().unwrap().is_busy());

    env.pick("DEEP");
    assert_eq!(env.events(), ["prompt:hold on"], "nothing switched while the turn runs");
    assert_eq!(env.picked(), Some("DEEP"), "the picker shows the pick");
    assert_eq!(env.role(), None);

    env.say("then this");
    assert_eq!(env.events(), ["prompt:hold on"], "queued behind the turn");

    env.release();
    assert_eq!(env.events(), ["prompt:hold on", "model:test/deep", format!("prompt:{NOTE_DEEP}then this").as_str()]);
    assert_eq!(env.role().as_deref(), Some("DEEP"));
    assert_eq!(env.picked(), Some("DEEP"));
}

#[test]
fn the_picker_follows_the_sessions_model_role() {
    let mut env = Env::new();
    env.start();
    env.say("hello");
    assert_eq!(env.picked(), Some("default"));

    env.say("/role DEEP");
    assert_eq!(env.role().as_deref(), Some("DEEP"));
    assert_eq!(env.picked(), Some("DEEP"));
    assert_eq!(env.app.ctx.notice(), Some("Model role DEEP: test/deep"));

    env.say("/role default");
    assert_eq!(env.picked(), Some("default"));
}

/// Reads omp's workflows, as opening the editor does, and closes it again.
fn load(app: &mut App) {
    command(app, "workflows.new");
    app.ctx.close_screen();
}

fn instructions(role: &str) -> String {
    format!("The active model role is `{role}`. Follow the RULES.md section for that role.")
}

#[test]
fn a_new_omp_session_is_told_its_role_at_start() {
    let mut env = Env::new();
    env.start();
    assert_eq!(env.log.lock().unwrap().configs.last().unwrap().instructions.as_deref(), Some(instructions("default").as_str()));


    let mut env = Env::new();
    load(&mut env.app);
    env.pick("DEEP");
    env.start();
    assert_eq!(env.log.lock().unwrap().configs.last().unwrap().instructions.as_deref(), Some(instructions("DEEP").as_str()));
}

#[test]
fn a_mid_session_switch_is_noted_once_in_the_next_prompt() {
    let mut env = Env::new();
    env.start();
    env.say("hello");
    env.pick("DEEP");
    env.say("next");
    env.say("and again");
    assert_eq!(env.events(), ["prompt:hello", "model:test/deep", format!("prompt:{NOTE_DEEP}next").as_str(), "prompt:and again"]);
}

#[test]
fn a_queued_prompt_waits_for_its_sessions_role_switch() {
    let mut env = Env::new();
    env.start();
    env.say("hold on");
    env.pick("DEEP");
    env.say("queued");
    // The role is gone from omp's config by the time the turn ends.
    std::fs::write(env.agent_dir.join("config.yml"), "modelRoles:\n  default: test/fast\n").unwrap();
    env.release();
    assert_eq!(env.events(), ["prompt:hold on"], "the switch failed, so nothing is sent");
    assert_eq!(env.app.ctx.composer.queue().count(), 1, "the prompt stays queued");
    assert!(env.app.ctx.notice().is_some_and(|notice| notice.contains("stays queued")), "{:?}", env.app.ctx.notice());

    // A switch that works releases it, with the note.
    std::fs::write(env.agent_dir.join("config.yml"), CONFIG).unwrap();
    env.pick("DEEP");
    assert_eq!(env.events(), ["prompt:hold on", "model:test/deep", format!("prompt:{NOTE_DEEP}queued").as_str()]);
    assert_eq!(env.app.ctx.composer.queue().count(), 0);
}

#[test]
fn a_held_prompt_never_goes_to_another_session() {
    let mut env = Env::new();
    env.start();
    env.say("hold on");
    env.pick("DEEP");
    env.say("queued");
    std::fs::write(env.agent_dir.join("config.yml"), "modelRoles:\n  default: test/fast\n").unwrap();
    env.release();
    assert_eq!(env.events(), ["prompt:hold on"]);

    // Another session comes up; the first one's queue doesn't follow.
    env.send(AppMessage::NewSession);
    std::fs::write(env.agent_dir.join("config.yml"), CONFIG).unwrap();
    env.pick("DEEP");
    assert!(env.events().iter().all(|event| !event.contains("queued")), "{:?}", env.events());
}

#[test]
fn use_workflow_commands_pick_a_workflow_from_the_keyboard() {
    let mut env = Env::new();
    load(&mut env.app);
    env.start();
    env.say("hello");
    let command = palette_command(&mut env.app, "Use workflow: Deep").expect("listed");
    assert!(command.enabled);
    assert!(palette_command(&mut env.app, "Use workflow: Default").expect("listed").enabled);
    env.app.ctx.close_screen();
    command_run(&mut env, "workflows.use.DEEP");
    assert_eq!(env.events(), ["prompt:hello", "model:test/deep"]);
    assert_eq!(env.role().as_deref(), Some("DEEP"));
}

fn command_run(env: &mut Env, id: &str) {
    command(&mut env.app, id);
    env.pump();
}

#[test]
fn use_workflow_commands_are_dimmed_for_other_harnesses() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    fix(&mut app, &agent_dir(dir.path()));
    update(&mut app, AppMessage::Workflows(Message::Open));
    assert!(!palette_command(&mut app, "Use workflow: Default").expect("listed").enabled);
    app.ctx.clear_notice();
    command(&mut app, "workflows.use.DEEP");
    assert_eq!(app.ctx.notice(), Some(OMP_ONLY));
}

#[test]
fn the_role_lookup_reads_the_directory_the_workflows_area_resolved() {
    let mut env = Env::new();
    let other = env._dir.path().join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("config.yml"), "modelRoles:\n  ALT: test/alt\n").unwrap();
    env.app.ctx.services.omp_agent_dir.set(other);
    env.start();
    assert!(palette_command(&mut env.app, "Model role: ALT (test/alt)").is_some());
    assert!(palette_command(&mut env.app, "Model role: DEEP (test/deep)").is_none());
}

#[test]
fn the_picker_is_rebuilt_only_when_its_inputs_change() {
    let mut env = Env::new();
    load(&mut env.app);
    let revision = env.app.areas.workflows.revision;
    command(&mut env.app, "workflows.new");
    assert_eq!(env.app.areas.workflows.revision, revision, "the same list again changes nothing");
    assert!(!env.app.areas.workflows.picker_changed("default", true));
    assert!(env.app.areas.workflows.picker_changed("DEEP", true), "another role");
    assert!(env.app.areas.workflows.picker_changed("DEEP", false), "another harness state");
    env.app.ctx.close_screen();
    create_research(&mut env.app);
    assert!(env.app.areas.workflows.revision > revision);
    assert!(env.app.areas.workflows.picker_changed("DEEP", false), "a new list");
}

#[test]
fn a_read_that_finishes_after_the_harness_changed_shows_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let agent_dir = agent_dir(dir.path());
    fix(&mut app, &agent_dir);
    let data = Data { agent_dir, workflows: Vec::new(), agents: Vec::new(), models: HashMap::new() };
    update(&mut app, AppMessage::Workflows(Message::Loaded { intent: Intent::New, result: Ok(data) }));
    assert_eq!(app.ctx.screen(), None);
    assert_eq!(app.ctx.notice(), Some(OMP_ONLY));
}

#[test]
fn saving_and_deleting_are_refused_once_the_harness_isn_omp() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake-a"]);
    let agent_dir = agent_dir(dir.path());
    fix(&mut app, &agent_dir);
    let rules = std::fs::read_to_string(agent_dir.join("RULES.md")).unwrap();
    fill(&mut app, screen::Message::Name("Research".into()));
    fill(&mut app, screen::Message::Save);
    assert_eq!(app.ctx.notice(), Some(OMP_ONLY));
    app.ctx.clear_notice();
    let event = screen::Event::Delete { role: "DEEP".into() };
    let task = State::screen_event(&mut app.areas.workflows, event, &mut app.ctx);
    drop(task);
    assert_eq!(app.ctx.notice(), Some(OMP_ONLY));
    assert_eq!(std::fs::read_to_string(agent_dir.join("RULES.md")).unwrap(), rules);
}
