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
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use iced::{Element, Subscription, Task};
use loa_bus::{Listing, SessionId};
use loa_contracts::harness::{Answer, Capabilities, Prompt, SessionConfig};
use loa_core::{Dom, ElementKind, JobStatus, NodeId, Op, Patch, SessionMode, TurnStatus, attr};
use loa_session::hooks::TurnHooks;
use loa_session::plan::{self as plan_mode, PlanGate};
use loa_settings::models::login::UrlOpener;
use loa_ui_panel::{agents, session_tree};
use loa_ui_session::transcript as chat;
use loa_view::session::composer::{Command, ModelOption};
use loa_view::session::transcript;
use loa_view::panel::{AgentsIntent, SessionTreeIntent};
use loa_contracts::session::SessionSummary;

use crate::area::{Area, AreaCommand, AreaMessage, Ctx, Listed, PanelTab, Request, Target, perform};
use loa_runtime::session::{Session, SessionEvent};
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
    Chat(transcript::Intent),
    /// The user picked a mode in the composer.
    Mode(SessionMode),
    /// Delete a session and its journal.
    Delete(String),
    /// Answer to a plan review the app raised, from the decision card.
    AnswerPlan { session: SessionId, decision: NodeId, answer: Answer },
    PlanAnswered { session: SessionId, decision: NodeId, result: Result<PlanNext, String> },
    /// The advisor reviewed `turn`.
    Advised { session: SessionId, turn: NodeId, result: Result<Option<String>, String> },
    /// A role switch finished: the role and what it switched to.
    RoleSet { session: String, result: Result<(String, loa_session::RoleSelected), String> },
    /// Where omp's `config.yml` is, for the model roles `/role` and the palette offer.
    ConfigLocated(Result<PathBuf, String>),
    Transfer(transfer::Message),
    /// Current time, unix ms, for the Agents tab.
    Tick(i64),
    /// A harness call returned.
    Done(Result<(), String>),
    /// A Session tree read off the UI thread finished.
    TreeLoaded(Box<tree::Loaded>),
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
    /// Roles picked while the session ran a turn or switched roles: applied once it is idle.
    pending_roles: HashMap<String, String>,
    switching_roles: HashSet<String>,
    /// Sessions whose composer holds its queued prompts until their role switch ends.
    held: HashSet<String>,
    /// The role each session's next prompt announces.
    role_notes: HashMap<String, String>,
    /// omp's `config.yml`, or why it couldn't be located; `None` until asked.
    config: Option<Result<PathBuf, String>>,
    /// Its model roles, cached by its modified time.
    roles: RefCell<Option<(SystemTime, Roles)>>,
}

/// omp's model roles with their candidate models in order.
type Roles = Vec<(String, Vec<String>)>;

impl State {
    /// Rebuilds from the active session.
    fn reload(&mut self, ctx: &mut Ctx) -> Task<Message> {
        let empty = Dom::new();
        let dom = ctx.runtime.sessions().active().map_or(&empty, |session| session.dom());
        self.agents.reset(dom);
        let _ = self.agents.update(agents::Message::Tick(loa_core::now_ms()));
        ctx.composer.set_mode(mode(dom));
        ctx.composer.set_commands(composer_commands(&ctx.slash, ctx.runtime.sessions().active().map(|session| session.dom())));
        let task = self.tree.refresh(ctx);
        self.restore_plan(ctx);
        task
    }

    /// A streamed append to session `id`; only the Agents tab follows those.
    pub(crate) fn on_append(&mut self, id: &SessionId, patch: &Patch, ctx: &Ctx) {
        if let Some(session) = ctx.runtime.sessions().get(id.as_str()).filter(|_| ctx.runtime.sessions().is_active(id.as_str())) {
            self.agents.apply(session.dom(), patch);
        }
    }

    pub(crate) fn role_switch_pending(&self, id: &str) -> bool {
        self.pending_roles.contains_key(id) || self.switching_roles.contains(id)
    }

    /// At the end of `id`'s turn: holds its queued prompts while a role switch is due for it.
    pub(crate) fn hold(&mut self, id: &str) -> bool {
        let due = self.role_switch_pending(id);
        if due {
            self.held.insert(id.to_owned());
        }
        due
    }

    /// The role switches of `id` are over, whatever their outcome: the prompts held for them go out.
    fn release(&mut self, ctx: &mut Ctx, id: &str) {
        if self.held.remove(id) {
            ctx.request(Request::ReleaseRoleQueue(id.to_owned()));
        }
    }

    /// omp's model roles, read again when `config.yml` changed; `None` until it is located.
    fn roles(&self) -> Option<Result<Roles, String>> {
        let path = match self.config.as_ref()? {
            Ok(path) => path,
            Err(error) => return Some(Err(format!("Can't find omp's config: {error}"))),
        };
        let mtime = std::fs::metadata(path).and_then(|meta| meta.modified()).ok();
        if let (Some(mtime), Some((cached, roles))) = (mtime, self.roles.borrow().as_ref())
            && *cached == mtime
        {
            return Some(Ok(roles.clone()));
        }
        let roles = model_roles(path);
        if let (Some(mtime), Ok(roles)) = (mtime, &roles) {
            *self.roles.borrow_mut() = Some((mtime, roles.clone()));
        }
        Some(roles)
    }

    fn locate_config(ctx: &Ctx) -> Task<Message> {
        let backend = ctx.backends.models.clone();
        Task::perform(async move { backend.files().await.map(|files| files.config()) }, Message::ConfigLocated)
    }

    fn delete(&mut self, ctx: &mut Ctx, id: &str) -> Task<Message> {
        self.tree.forget(id);
        self.plans.remove(id);
        self.hooks.locked().remove(id);
        if ctx.runtime.sessions().get(id).is_some() {
            ctx.discard_session(id);
        } else {
            match ctx.runtime.library().delete(id) {
                Ok(()) => ctx.shell.sidebar_mut().remove_session(id),
                Err(error) => ctx.notify(format!("Can't delete session {id}: {error}")),
            }
        }
        if ctx.shell.sidebar().session(id).is_none() {
            // Drops the session from the sidebar's pins and the saved ones.
            if ctx.shell.sidebar().pins().contains(id) {
                let pins: Vec<String> = ctx.shell.sidebar().pins().iter().filter(|pin| *pin != id).cloned().collect();
                ctx.shell.sidebar_mut().set_pins(pins);
            }
            ctx.edit_config("pins", |config| config.pinned.remove(id));
        }
        Task::none()
    }

    /// Switches the active session to one of omp's model roles.
    fn role(&mut self, ctx: &mut Ctx, role: String) -> Task<Message> {
        let Some((id, running, busy)) = ctx.runtime.sessions().active().map(|session| (session.id().to_string(), session.harness().is_some(), session.is_busy())) else {
            ctx.notify("Open a session to switch its model role");
            return Task::none();
        };
        if !running {
            ctx.notify("Model roles switch a running session: send a message to resume it first");
            return Task::none();
        }
        match self.roles() {
            Some(Err(error)) => {
                ctx.notify(error);
                return Task::none();
            }
            Some(Ok(roles)) if !roles.iter().any(|(name, _)| *name == role) => {
                let names: Vec<&str> = roles.iter().map(|(name, _)| name.as_str()).collect();
                ctx.notify(if role.is_empty() {
                    format!("Name a model role: /role <{}>", names.join("|"))
                } else {
                    format!("omp has no model role {role}; its roles: {}", names.join(", "))
                });
                return Task::none();
            }
            None if role.is_empty() => {
                ctx.notify("Name a model role: /role <name>");
                return Task::none();
            }
            _ => {}
        }
        if self.switching_roles.contains(&id) || busy {
            self.pending_roles.insert(id, role);
            return Task::none();
        }
        self.switch_role(ctx, id, role)
    }

    fn switch_role(&mut self, ctx: &mut Ctx, id: String, role: String) -> Task<Message> {
        let running = ctx.runtime.sessions().get(&id).and_then(|session| Some((session.harness()?.clone(), session.dom().root()?.id, Arc::clone(session.writer()))));
        let Some((harness, root, writer)) = running else {
            ctx.notify(format!("Can't switch to model role {role}: the session no longer runs"));
            self.release(ctx, &id);
            return Task::none();
        };
        // Prompts sent while the switch runs wait for it, so the first of them carries the role note.
        if ctx.runtime.sessions().is_active(&id) && !ctx.composer.is_busy() {
            ctx.composer.turn_started();
            self.held.insert(id.clone());
        }
        let capabilities = capabilities(ctx, harness.harness());
        let backend = ctx.backends.models.clone();
        self.switching_roles.insert(id.clone());
        Task::perform(async move {
            let files = backend.files().await?;
            let roles = model_roles(&files.config())?;
            let Some((_, candidates)) = roles.iter().find(|(name, _)| *name == role) else {
                let names: Vec<&str> = roles.iter().map(|(name, _)| name.as_str()).collect();
                return Err(format!("omp has no model role {role}; its roles: {}", names.join(", ")));
            };
            loa_session::select_role(harness.as_ref(), &capabilities, writer.as_ref(), root, &role, candidates)
                .await.map(|selected| (role, selected)).map_err(|error| error.to_string())
        }, move |result| Message::RoleSet { session: id.clone(), result })
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
fn composer_commands(app: &[Command], dom: Option<&Dom>) -> Vec<Command> {
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
            commands.push(Command { name: name.to_owned(), description: format!("{harness}: {description}") });
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

/// Reads omp's model roles, candidates in order, from `config.yml` alone.
pub(crate) fn model_roles(path: &Path) -> Result<Roles, String> {
    let config = loa_settings::models::config::OmpConfig::load(&path).map_err(|error| format!("Can't read {}: {error}", path.display()))?;
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
                let option = ModelOption { id: model.to_owned(), name: model.to_owned(), provider: String::new(), role: None };
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
            if ctx.runtime.sessions().active().is_none() { return Vec::new(); }
            state.roles().and_then(Result::ok).unwrap_or_default().into_iter().map(|(role, models)| {
                let title = format!("Model role: {role} ({})", models.first().map_or("no model", String::as_str));
                (role, title)
            }).collect()
        })),
    ];

    type Message = Message;

    fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let home = ctx.backends.home.clone();
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
            opener: Arc::clone(&ctx.backends.opener),
            branching: None,
            pending_roles: HashMap::new(),
            switching_roles: HashSet::new(),
            held: HashSet::new(),
            role_notes: HashMap::new(),
            config: None,
            roles: RefCell::new(None),
        };
        let task = Self::locate_config(ctx);
        (state, task)
    }

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Agents(message) => {
                let (task, event) = self.agents.update(message);
                let child = match event {
                    Some(AgentsIntent::OpenSession(child)) => self.child_session(ctx, child).map(Request::OpenSession),
                    Some(AgentsIntent::DetachSession(child)) => self.child_session(ctx, child).map(Request::DetachSession),
                    // Harnesses stop jobs only with the turn that runs them.
                    Some(AgentsIntent::Cancel { .. }) => match ctx.runtime.sessions().active().and_then(|session| session.harness().cloned()) {
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
                    Some(SessionTreeIntent::Open(id)) => ctx.request(Request::OpenSession(id)),
                    Some(SessionTreeIntent::Fork { session, turn }) => self.fork_at(ctx, &session, turn),
                    Some(SessionTreeIntent::Rewind { session, turn }) => self.rewind(ctx, &session, turn),
                    None => {}
                }
                task.map(Message::Tree)
            }
            Message::Chat(transcript::Intent::Rewind(node)) => {
                if let Some(id) = ctx.runtime.sessions().active_id().map(ToString::to_string) {
                    self.rewind(ctx, &id, node);
                }
                Task::none()
            }
            Message::Chat(transcript::Intent::OpenLink(link)) => {
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
            Message::ConfigLocated(result) => {
                self.config = Some(result);
                Task::none()
            }
            Message::RoleSet { session, result } => {
                self.switching_roles.remove(&session);
                if let Some((role, selected)) = ctx.report("Can't switch the model role", result) {
                    self.role_notes.insert(session.clone(), role.clone());
                    ctx.notify(match selected.effort_error {
                        Some(error) => format!("Model role {role}: {}; {error}", selected.model),
                        None => format!("Model role {role}: {}", selected.model),
                    });
                }
                if let Some(role) = self.pending_roles.remove(&session) {
                    return self.switch_role(ctx, session, role);
                }
                self.release(ctx, &session);
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
            Message::TreeLoaded(loaded) => {
                self.tree.merge(*loaded);
                self.tree.refresh(ctx)
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
        if let Some(role) = self.role_notes.remove(session.as_str()) {
            prompt.text = format!("The active model role is now `{role}`.\n\n{}", prompt.text);
        }
        self.hook_prompt(ctx, session, prompt);
        if self.gate.active(session.as_str()) && !prompt.text.contains(plan_mode::PLAN_STEP) {
            prompt.text = plan_mode::wrap_prompt(&prompt.text);
        }
    }

    fn on_session_event(&mut self, event: &SessionEvent, ctx: &mut Ctx) -> Task<Message> {
        match event {
            SessionEvent::Activated(id) => {
                if let Some(old) = id.as_ref().and_then(|id| self.compacted.remove(id.as_str())) {
                    // The compacted session's harness already stopped: closing must not stop it again.
                    drop(ctx.runtime.sessions_mut().get_mut(&old).and_then(Session::detach));
                    ctx.replace_session(&old, id.clone(), false);
                }
                if id.is_some() && id.as_ref().map(SessionId::as_str) == self.branching.as_deref() {
                    self.branching = None;
                    ctx.reveal_tab("session");
                }
                let locate = if matches!(self.config, Some(Ok(_))) { Task::none() } else { Self::locate_config(ctx) };
                return Task::batch([locate, self.reload(ctx)]);
            }
            SessionEvent::Loaded(id) if ctx.runtime.sessions().is_active(id.as_str()) => return self.reload(ctx),
            SessionEvent::Patch(id, patch) => {
                let sessions = ctx.runtime.sessions();
                if let Op::Insert { parent, kind: ElementKind::Turn, .. } = &patch.op
                    && sessions.get(id.as_str()).and_then(|session| session.dom().root()).is_some_and(|root| root.id == *parent)
                {
                    self.tree.record_turn(id.as_str(), patch.node, patch.seq);
                }
                let Some(dom) = sessions.get(id.as_str()).filter(|_| sessions.is_active(id.as_str())).map(|session| session.dom()) else { return Task::none() };
                self.agents.apply(dom, patch);
                let ext = |node| dom.get(node).and_then(|node| node.str_attr(attr::TYPE)) == Some(chat::COMMANDS_EXT);
                match &patch.op {
                    Op::Insert { kind: ElementKind::Job, .. } => ctx.reveal_tab("agents"),
                    Op::Insert { kind: ElementKind::Ext, .. } if ext(patch.node) => ctx.composer.set_commands(composer_commands(&ctx.slash, Some(dom))),
                    Op::Set { name, .. } if name == attr::MODE && dom.root().is_some_and(|root| root.id == patch.node) => ctx.composer.set_mode(mode(dom)),
                    _ => {}
                }
            }
            SessionEvent::TurnStarted { session, .. } if ctx.runtime.sessions().is_active(session.as_str()) => return self.tree.refresh(ctx),
            SessionEvent::DecisionWaiting { session, decision } => {
                let Some(open) = ctx.runtime.sessions().get(session.as_str()) else { return Task::none() };
                let (Some(node), Some(harness)) = (open.dom().get(*decision), open.harness().cloned()) else { return Task::none() };
                if let Some(answer) = self.gate.verdict(session.as_str(), node) {
                    let decision = *decision;
                    return perform(async move { harness.answer(decision, answer).await }, Message::Done);
                }
            }
            SessionEvent::TurnEnded { session, turn, status } => {
                let switch = self.pending_roles.remove(session.as_str()).map_or_else(Task::none, |role| self.switch_role(ctx, session.to_string(), role));
                let advice = if *status == TurnStatus::Done {
                    if self.gate.active(session.as_str()) {
                        self.submit_plan(ctx, session, *turn);
                    }
                    self.advise(ctx, session, *turn)
                } else {
                    Task::none()
                };
                return Task::batch([switch, advice]);
            }
            SessionEvent::Closed(id) => {
                self.plans.remove(id.as_str());
                self.hooks.locked().remove(id.as_str());
                self.pending_roles.remove(id.as_str());
                self.switching_roles.remove(id.as_str());
                self.held.remove(id.as_str());
                self.role_notes.remove(id.as_str());
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
