//! Session features around the chat: the Agents and Session tree tabs, branching, plan mode,
//! rules and the advisor, model roles, import and export.

mod branch;
mod hooks;
mod plan;
mod transfer;
mod tree;
#[cfg(test)]
mod tests;

use loa_util::LockExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iced::{Element, Subscription, Task};
use loa_bus::{Listing, SessionId};
use loa_contracts::harness::{Answer, Capabilities, Prompt, SessionConfig};
use loa_core::{Dom, ElementKind, JobStatus, NodeId, Op, SessionMode, TurnStatus, attr};
use loa_harness::account_support::opener::SystemUrlOpener;
use loa_session::hooks::TurnHooks;
use loa_session::plan::{self as plan_mode, PlanGate};
use loa_models::login::UrlOpener;
use loa_ui_panel::{agents, session_tree};
use loa_ui_session::composer;
use loa_ui_session::transcript as chat;
use loa_contracts::session::SessionSummary;

use crate::area::{Area, AreaCommand, AreaMessage, Ctx, Listed, PanelTab, Request, Target, perform};
use loa_runtime::session::SessionEvent;
pub use plan::{Held, PlanNext};

/// Model or omp role of the app's small-model calls: compaction summaries and advisor notes.
const SMALL_MODEL: &str = "@smol";
/// How often the Agents tab advances elapsed times while a job runs.
const TICK: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub enum Message {
    Agents(agents::Message),
    Tree(session_tree::Message),
    /// Something the user did in the transcript.
    Chat(chat::Event),
    /// The user picked a mode in the composer.
    Mode(SessionMode),
    /// Delete a session and its journal.
    Delete(String),
    /// Answer to a plan review the app raised, from the decision card.
    AnswerPlan { session: SessionId, decision: NodeId, answer: Answer },
    PlanAnswered { session: SessionId, decision: NodeId, result: Result<PlanNext, String> },
    /// The advisor reviewed `turn`.
    Advised { session: SessionId, turn: NodeId, result: Result<Option<String>, String> },
    /// A role switch finished: the role and the model picked.
    RoleSet(Result<(String, String), String>),
    Transfer(transfer::Message),
    /// Current time, unix ms, for the Agents tab.
    Tick(i64),
    /// A harness call returned.
    Done(Result<(), String>),
}

pub struct State {
    agents: agents::State,
    tree: tree::Tree,
    transfer: transfer::State,
    /// Read-only tool gate of app plan mode.
    gate: Arc<PlanGate>,
    plans: HashMap<String, plan::Plan>,
    /// Rules and advisor state per session whose harness lacks them.
    hooks: Mutex<HashMap<String, TurnHooks>>,
    /// The small model; `None` when omp, which runs it, isn't installed.
    small: Option<Arc<dyn loa_projects::Summarizers>>,
    /// Home directory: native sessions, omp's rules and config.
    pub(crate) home: PathBuf,
    /// Compacted successor id -> the session it replaces.
    compacted: HashMap<String, String>,
    /// Opens links clicked in the transcript.
    pub(crate) opener: Arc<dyn UrlOpener>,
    /// The fork or rewind being started, to show the Session tab once it is active.
    branching: Option<String>,
}

impl State {
    /// Rebuilds from the active session.
    fn reload(&mut self, ctx: &mut Ctx) {
        let empty = Dom::new();
        let dom = ctx.runtime.sessions().active().map_or(&empty, |session| session.dom());
        self.agents.reset(dom);
        let _ = self.agents.update(agents::Message::Tick(loa_core::now_ms()));
        ctx.composer.set_mode(mode(dom));
        ctx.composer.set_commands(composer_commands(&ctx.slash, ctx.runtime.sessions().active().map(|session| session.dom())));
        self.tree.refresh(ctx);
        self.restore_plan(ctx);
    }

    fn delete(&mut self, ctx: &mut Ctx, id: &str) -> Task<Message> {
        self.tree.forget(id);
        self.plans.remove(id);
        self.hooks.locked().remove(id);
        let task = if ctx.runtime.sessions().get(id).is_some() {
            ctx.discard_session(id).map_or_else(Task::none, |harness| perform(async move { harness.shutdown().await }, Message::Done))
        } else {
            match ctx.runtime.library().delete(id) {
                Ok(()) => ctx.shell.sidebar_mut().remove_session(id),
                Err(error) => ctx.notify(format!("Can't delete session {id}: {error}")),
            }
            Task::none()
        };
        if ctx.shell.sidebar().session(id).is_none() {
            // Drops the session from the sidebar's pins and the saved ones.
            if ctx.shell.sidebar().pins().contains(id) {
                let pins: Vec<String> = ctx.shell.sidebar().pins().iter().filter(|pin| *pin != id).cloned().collect();
                ctx.shell.sidebar_mut().set_pins(pins);
            }
            ctx.edit_config("pins", |config| config.pinned.remove(id));
        }
        task
    }

    /// Switches the active session to one of omp's model roles.
    fn role(&mut self, ctx: &mut Ctx, role: String) -> Task<Message> {
        let Some(session) = ctx.runtime.sessions().active() else {
            ctx.notify("Open a session to switch its model role");
            return Task::none();
        };
        let Some(harness) = session.harness().cloned() else {
            ctx.notify("Model roles switch a running session: send a message to resume it first");
            return Task::none();
        };
        let Some(root) = session.dom().root().map(|root| root.id) else { return Task::none() };
        let writer = Arc::clone(session.writer());
        let roles = match model_roles(&self.home) {
            Ok(roles) => roles,
            Err(error) => {
                ctx.notify(error);
                return Task::none();
            }
        };
        let Some((_, candidates)) = roles.iter().find(|(name, _)| *name == role) else {
            let names: Vec<&str> = roles.iter().map(|(name, _)| name.as_str()).collect();
            ctx.notify(if role.is_empty() {
                format!("Name a model role: /role <{}>", names.join("|"))
            } else {
                format!("omp has no model role {role}; its roles: {}", names.join(", "))
            });
            return Task::none();
        };
        let (capabilities, candidates) = (capabilities(ctx, harness.harness()), candidates.clone());
        perform(
            async move { loa_session::select_role(harness.as_ref(), &capabilities, writer.as_ref(), root, &role, &candidates).await.map(|model| (role, model)) },
            Message::RoleSet,
        )
    }

    /// The library id of a subagent's session: stored already, or imported from its harness's file here.
    fn child_session(&mut self, ctx: &mut Ctx, child: String) -> Option<String> {
        let listings = ctx.runtime.library().store().list();
        if listings.is_ok_and(|listings| listings.iter().any(|listing| matches!(listing, Listing::Session(info) if info.id == child))) {
            return Some(child);
        }
        let path = PathBuf::from(&child);
        if !path.is_file() {
            ctx.notify(format!("The subagent's session {child} isn't on this machine"));
            return None;
        }
        let parent = ctx.runtime.sessions().active()?;
        let parent_id = parent.id().to_string();
        let Some(adapter) = parent.harness_id().and_then(|harness| ctx.runtime.harnesses().registry.get(harness)) else {
            ctx.notify("The parent session's harness isn't supported here");
            return None;
        };
        let job = self.agents.jobs().find(|job| job.session() == Some(child.as_str())).map_or(NodeId::DOCUMENT, agents::Job::node);
        let imported = ctx.runtime.library().import_child(adapter.as_ref(), &parent_id, job, &path, &ctx.services.node.key);
        ctx.report("Can't open the subagent's session", imported)
    }

    fn open_link(&self, ctx: &mut Ctx, link: &str) {
        if !(link.starts_with("https://") || link.starts_with("http://")) {
            ctx.notify(format!("Only web links open in the browser: {link}"));
        } else if let Err(error) = self.opener.open_url(link) {
            ctx.notify(format!("Can't open {link}: {error}"));
        }
    }
}

/// The composer's `/` commands: the app's, then the harness's own from the latest list its adapter recorded in `dom`.
fn composer_commands(app: &[composer::Command], dom: Option<&Dom>) -> Vec<composer::Command> {
    let mut commands = app.to_vec();
    let listed = dom.and_then(|dom| dom.iter().filter(|node| node.kind == ElementKind::Ext && node.str_attr(attr::TYPE) == Some(chat::COMMANDS_EXT)).max_by_key(|node| node.id));
    let Some(listed) = listed else { return commands };
    let harness = listed.str_attr(attr::HARNESS).unwrap_or("harness");
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&listed.text) else { return commands };
    // omp's `commands`, ACP's `availableCommands`.
    let list = json.get("commands").or_else(|| json.get("availableCommands")).and_then(serde_json::Value::as_array);
    for command in list.into_iter().flatten() {
        let Some(name) = command.get("name").and_then(serde_json::Value::as_str) else { continue };
        if !commands.iter().any(|known| known.name == name) {
            let description = command.get("description").and_then(serde_json::Value::as_str).unwrap_or_default();
            commands.push(composer::Command { name: name.to_owned(), description: format!("{harness}: {description}") });
        }
    }
    commands
}

/// The session's mode, `normal` unless the DOM says otherwise.
fn mode(dom: &Dom) -> SessionMode {
    dom.root().and_then(|root| root.str_attr(attr::MODE)).and_then(SessionMode::parse).unwrap_or(SessionMode::Normal)
}

/// The session's title as the sidebar shows it.
fn title_of(dom: &Dom) -> String {
    SessionSummary::from_dom(dom, false).map_or_else(|| "session".to_owned(), |summary| summary.title)
}

fn capabilities(ctx: &Ctx, harness: &str) -> Capabilities {
    ctx.runtime.harnesses().registry.get(harness).map(|adapter| adapter.capabilities()).unwrap_or_default()
}

/// Whether `harness` runs on an approval mode that asks before tools that write or execute.
fn asks_first(ctx: &Ctx, harness: &str) -> bool {
    !ctx.runtime.harnesses().traits(harness).approval_modes.is_empty()
}

/// omp's model roles (`modelRoles` in `~/.omp/agent/config.yml`) with their candidate models in order.
fn model_roles(home: &Path) -> Result<Vec<(String, Vec<String>)>, String> {
    let path = home.join(".omp/agent/config.yml");
    let config = loa_models::config::OmpConfig::load(&path).map_err(|error| format!("Can't read {}: {error}", path.display()))?;
    let roles = config.model_roles().map_err(|error| format!("Can't read the model roles in {}: {error}", path.display()))?;
    Ok(roles.into_iter().map(|role| (role.role, role.candidates.iter().map(ToString::to_string).collect())).collect())
}

impl AreaMessage for Message {
    type Area = State;
}

impl Area for State {
    const ID: &'static str = "sessions";
    const COMMANDS: &'static [AreaCommand<Self>] = &[
        AreaCommand::<Self>::new("sessions.model", "", "", |_, model, ctx| {
            if model.is_empty() {
                ctx.notify("Name the model: /model <provider/model>, or pick one in the model picker");
            } else {
                let option = composer::ModelOption { id: model.to_owned(), name: model.to_owned(), provider: String::new(), role: None };
                ctx.request(Request::Forward(Box::new(crate::app::Message::area(crate::areas::models::Message::Model(option)))));
            }
            Task::none()
        })
        .listed(Listed::Never)
        .slash("model", "Switch this session's model: /model <provider/model>"),
        AreaCommand::<Self>::new("sessions.role", "", "", |state, role, ctx| state.role(ctx, role.to_owned()))
            .listed(Listed::Never)
            .slash("role", "Switch to one of omp's model roles: /role <name>"),
        // Entries sharing an id run the same command, listed once in the palette's and once in the `/` list's order.
        AreaCommand::<Self>::new("sessions.import", "Import native session…", "Session", |state, _, ctx| {
            state.show_import(ctx);
            Task::none()
        }),
        AreaCommand::<Self>::new("sessions.compact", "", "", |state, instructions, ctx| state.compact(ctx, instructions))
            .listed(Listed::Never)
            .slash("compact", "Summarize the session and continue with the summary"),
        AreaCommand::<Self>::new("sessions.fork", "Fork session", "Session", |state, _, ctx| {
            state.fork_active(ctx);
            Task::none()
        })
        .listed(Listed::Session)
        .slash("fork", "Fork the session into a new one"),
        AreaCommand::<Self>::new("sessions.compact", "Compact session", "Session", |state, instructions, ctx| state.compact(ctx, instructions)).listed(Listed::Session),
        AreaCommand::<Self>::new("sessions.plan", "", "Session", |state, _, ctx| {
            let next = if state.in_plan(ctx) { SessionMode::Normal } else { SessionMode::Plan };
            state.pick_mode(next, ctx)
        })
        .listed(Listed::When(|state, ctx| {
            let session = ctx.runtime.sessions().active()?;
            let plan = state.in_plan(ctx) || mode(session.dom()) == SessionMode::Plan;
            Some(if plan { "Leave plan mode" } else { "Enter plan mode" })
        })),
        AreaCommand::<Self>::new("sessions.mode.plan", "", "", |state, _, ctx| state.pick_mode(SessionMode::Plan, ctx))
            .listed(Listed::Never)
            .slash("plan", "Plan mode: read only until you approve a plan"),
        AreaCommand::<Self>::new("sessions.mode.normal", "", "", |state, _, ctx| state.pick_mode(SessionMode::Normal, ctx))
            .listed(Listed::Never)
            .slash("normal", "Leave plan mode"),
        AreaCommand::<Self>::new("sessions.delete", "Delete session", "Session", |state, _, ctx| match ctx.runtime.sessions().active_id().map(ToString::to_string) {
            Some(id) => state.delete(ctx, &id),
            None => Task::none(),
        })
        .listed(Listed::Session),
        AreaCommand::<Self>::new("sessions.export", "Export session as native file", "Session", |state, _, ctx| {
            state.export(ctx, false, None);
            Task::none()
        })
        .listed(Listed::Session)
        .slash("export", "Export the session as a native file"),
        AreaCommand::<Self>::new("sessions.export-xml", "Export session as XML (debug)", "Session", |state, _, ctx| {
            state.export(ctx, true, None);
            Task::none()
        })
        .listed(Listed::Session)
        .slash("export-xml", "Export the session as XML for debugging"),
        AreaCommand::<Self>::new("sessions.import", "", "", |state, _, ctx| {
            state.show_import(ctx);
            Task::none()
        })
        .listed(Listed::Never)
        .slash("import", "Import a native session"),
        AreaCommand::<Self>::new("sessions.role.", "", "Model roles", |state, role, ctx| state.role(ctx, role.to_owned())).listed(Listed::Each(|state, ctx| {
            let roles = ctx.runtime.sessions().active().and_then(|_| model_roles(&state.home).ok()).unwrap_or_default();
            let title = |role: &str, models: &[String]| format!("Model role: {role} ({})", models.first().map_or("no model", String::as_str));
            roles.into_iter().map(|(role, models)| (role.clone(), title(&role, &models))).collect()
        })),
    ];

    type Message = Message;

    fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let home = loa_util::fs::home().unwrap_or_else(|| PathBuf::from("."));
        // Unit tests never make model calls: no small model.
        let small = if cfg!(test) {
            None
        } else {
            loa_util::exec::which("omp").map(|omp| Arc::new(crate::adapters::summarizer::OmpSummarizers(loa_harness::omp::OneShot::new(omp, ctx.runtime.cwd().to_path_buf()))) as Arc<dyn loa_projects::Summarizers>)
        };
        let state = Self {
            agents: agents::State::from_dom(&Dom::new()),
            tree: tree::Tree::default(),
            transfer: transfer::State::default(),
            gate: PlanGate::new(),
            plans: HashMap::new(),
            hooks: Mutex::default(),
            small,
            home,
            compacted: HashMap::new(),
            opener: Arc::new(SystemUrlOpener),
            branching: None,
        };
        (state, Task::none())
    }

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Agents(message) => {
                let (task, event) = self.agents.update(message);
                let child = match event {
                    Some(agents::Event::OpenSession(child)) => self.child_session(ctx, child).map(Request::OpenSession),
                    Some(agents::Event::DetachSession(child)) => self.child_session(ctx, child).map(Request::DetachSession),
                    // Harnesses stop jobs only with the turn that runs them.
                    Some(agents::Event::Cancel { .. }) => match ctx.runtime.sessions().active().and_then(|session| session.harness().cloned()) {
                        Some(harness) => {
                            ctx.notify("Interrupting the turn that runs this job");
                            return Task::batch([task.map(Message::Agents), perform(async move { harness.interrupt().await }, Message::Done)]);
                        }
                        None => None,
                    },
                    None => None,
                };
                child.into_iter().for_each(|request| ctx.request(request));
                task.map(Message::Agents)
            }
            Message::Tree(message) => {
                let (task, event) = self.tree.view.update(message);
                match event {
                    Some(session_tree::Event::Open(id)) => ctx.request(Request::OpenSession(id)),
                    Some(session_tree::Event::Fork { session, turn }) => self.fork_at(ctx, &session, turn),
                    Some(session_tree::Event::Rewind { session, turn }) => self.rewind(ctx, &session, turn),
                    None => {}
                }
                task.map(Message::Tree)
            }
            Message::Chat(chat::Event::Rewind(node)) => {
                if let Some(id) = ctx.runtime.sessions().active_id().map(ToString::to_string) {
                    self.rewind(ctx, &id, node);
                }
                Task::none()
            }
            Message::Chat(chat::Event::OpenLink(link)) => {
                self.open_link(ctx, &link);
                Task::none()
            }
            Message::Mode(mode) => self.pick_mode(mode, ctx),
            Message::Delete(id) => self.delete(ctx, &id),
            Message::AnswerPlan { session, decision, answer } => self.answer_plan(ctx, session, decision, answer),
            Message::PlanAnswered { session, decision, result } => {
                self.plan_answered(ctx, session, decision, result);
                Task::none()
            }
            Message::Advised { session, turn, result } => {
                self.advised(ctx, &session, turn, result);
                Task::none()
            }
            Message::RoleSet(result) => {
                if let Some((role, model)) = ctx.report("Can't switch the model role", result) {
                    ctx.notify(format!("Model role {role}: {model}"));
                }
                Task::none()
            }
            Message::Transfer(message) => {
                self.transfer(ctx, message);
                Task::none()
            }
            Message::Tick(now) => self.agents.update(agents::Message::Tick(now)).0.map(Message::Agents),
            Message::Done(result) => {
                if let Err(error) = result {
                    ctx.notify(error);
                }
                Task::none()
            }
        }
    }

    fn panel_tabs(&self) -> Vec<PanelTab> {
        vec![PanelTab::of(&self.agents, false), PanelTab::of(&self.tree.view, false)]
    }

    fn view_tab<'a>(&'a self, id: &str, _: &'a Ctx) -> Option<Element<'a, Message>> {
        match id {
            "agents" => Some(self.agents.view().map(Message::Agents)),
            "session" => Some(self.tree.view.view().map(Message::Tree)),
            _ => None,
        }
    }

    fn view_screen<'a>(&'a self, ctx: &'a Ctx) -> Option<Element<'a, Message>> {
        let active = ctx.runtime.sessions().active().map(|session| (session.id().as_str(), session.harness_id().unwrap_or("unknown")));
        Some(self.transfer.view(active, loa_core::now_ms()).map(Message::Transfer))
    }

    fn contribute(&self, config: &mut SessionConfig, target: &Target<'_>, ctx: &Ctx) {
        if asks_first(ctx, target.harness) {
            self.gate.enforce_through_approvals(&config.session_id);
        }
        self.start_hooks(config, target);
    }

    fn before_send(&mut self, session: &SessionId, prompt: &mut Prompt, ctx: &mut Ctx) {
        self.hook_prompt(ctx, session, prompt);
        if self.gate.active(session.as_str()) && !prompt.text.contains(plan_mode::PLAN_STEP) {
            prompt.text = plan_mode::wrap_prompt(&prompt.text);
        }
    }

    fn on_session_event(&mut self, event: &SessionEvent, ctx: &mut Ctx) -> Task<Message> {
        match event {
            SessionEvent::Activated(id) => {
                if let Some(old) = id.as_ref().and_then(|id| self.compacted.remove(id.as_str())) {
                    // The compacted session's harness already stopped.
                    let _ = ctx.close_session(&old);
                }
                if id.is_some() && id.as_ref().map(SessionId::as_str) == self.branching.as_deref() {
                    self.branching = None;
                    ctx.reveal_tab("session");
                }
                self.reload(ctx);
            }
            SessionEvent::Loaded(id) if ctx.runtime.sessions().is_active(id.as_str()) => self.reload(ctx),
            SessionEvent::Patch(id, patch) if ctx.runtime.sessions().is_active(id.as_str()) => {
                let Some(dom) = ctx.runtime.sessions().get(id.as_str()).map(|session| session.dom()) else { return Task::none() };
                self.agents.apply(dom, patch);
                let ext = |node| dom.get(node).and_then(|node| node.str_attr(attr::TYPE)) == Some(chat::COMMANDS_EXT);
                match &patch.op {
                    Op::Insert { kind: ElementKind::Job, .. } => ctx.reveal_tab("agents"),
                    Op::Insert { kind: ElementKind::Ext, .. } if ext(patch.node) => ctx.composer.set_commands(composer_commands(&ctx.slash, Some(dom))),
                    Op::Set { name, .. } if name == attr::MODE && dom.root().is_some_and(|root| root.id == patch.node) => ctx.composer.set_mode(mode(dom)),
                    _ => {}
                }
            }
            SessionEvent::TurnStarted { session, .. } if ctx.runtime.sessions().is_active(session.as_str()) => self.tree.refresh(ctx),
            SessionEvent::DecisionWaiting { session, decision } => {
                let Some(open) = ctx.runtime.sessions().get(session.as_str()) else { return Task::none() };
                let (Some(node), Some(harness)) = (open.dom().get(*decision), open.harness().cloned()) else { return Task::none() };
                if let Some(answer) = self.gate.verdict(session.as_str(), node) {
                    let decision = *decision;
                    return perform(async move { harness.answer(decision, answer).await }, Message::Done);
                }
            }
            SessionEvent::TurnEnded { session, turn, status: TurnStatus::Done } => {
                if self.gate.active(session.as_str()) {
                    self.submit_plan(ctx, session, *turn);
                }
                return self.advise(ctx, session, *turn);
            }
            SessionEvent::Closed(id) => {
                self.plans.remove(id.as_str());
                self.hooks.locked().remove(id.as_str());
            }
            _ => {}
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        if self.agents.jobs().any(|job| job.status() == JobStatus::Running) {
            iced::time::every(TICK).map(|_| Message::Tick(loa_core::now_ms()))
        } else {
            Subscription::none()
        }
    }
}
