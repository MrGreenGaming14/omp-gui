//! Session features around the chat:
//!
//! - Recents: delete a session from its sidebar row or the palette
//! - fork (palette, Session tree) and rewind (a user message in the chat, the
//!   Session tree): a new session branched off through loa-features, natively
//!   when the harness forks, else a fresh session seeded with the history
//! - compaction (`/compact`, palette): native, else a small-model summary
//!   carried into a fresh session
//! - `/` commands in the composer: the app's (this area's, plus `/new` and
//!   `/model`, which the app runs) and the harness's own, listed from the
//!   commands its adapter records in the session
//! - model roles (palette, `/role`): switch the running session to a role's
//!   model
//! - the Agents tab (jobs, live, with omp's `wait`) and opening a subagent's
//!   linked child session; the tab shows up when a job starts, and the
//!   Session tree when a fork or rewind makes a branch
//! - links in the transcript open in the system browser
//! - plan mode on harnesses without it: read-only through the harness's tool
//!   approvals, then a plan review card
//! - rules and advisor around each turn on harnesses without them
//! - import of native sessions and export as native file or debugging XML

mod transfer;
mod tree;
#[cfg(test)]
mod tests;

use loa_util::LockExt;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iced::{Element, Subscription, Task};
use loa_contracts::harness::{Answer, Capabilities, Feature, Prompt, SessionConfig};
use loa_bus::SessionId;
use loa_core::{Dom, Draft, ElementKind, JobStatus, NodeId, Op, PatchSink, SessionMode, TurnStatus, answer, attr};
use loa_features::hooks::{self, Advisor, TurnHooks};
use loa_features::plan::{self as plan_mode, Outcome, PlanGate, Verdict};
use loa_features::smol::OneShot;
use loa_features::{Compaction, PlanPermit, native};
use loa_journal::Listing;
use loa_models::login::UrlOpener;
use loa_models::opener::SystemUrlOpener;
use loa_ui_chat as chat;
use loa_ui_composer as composer;
use loa_ui_palette as palette;
use loa_ui_panel::{agents, session_tree};
use loa_ui_shell::SessionSummary;

use crate::area::{Area, AreaId, Ctx, PanelTab, Request, SessionEvent, Spawn, SpawnStart, Target};
use crate::session::new_session_id;

/// Model or omp role of the app's small-model calls: compaction summaries
/// and advisor notes.
const SMALL_MODEL: &str = "@smol";
/// The `/compact` composer command.
const COMPACT: &str = "compact";
/// How often the Agents tab advances elapsed times while a job runs.
const TICK: Duration = Duration::from_secs(1);
const ROLE_COMMAND: &str = "role.";

/// The app's `/` commands, offered in the composer before the harness's
/// own. The app runs `/new` and `/model`; this area runs the rest
/// ([`slash`]).
const APP_COMMANDS: [(&str, &str); 10] = [
    ("new", "Start a new session"),
    ("model", "Switch this session's model: /model <provider/model>"),
    ("role", "Switch to one of omp's model roles: /role <name>"),
    (COMPACT, "Summarize the session and continue with the summary"),
    ("fork", "Fork the session into a new one"),
    ("plan", "Plan mode: read only until you approve a plan"),
    ("normal", "Leave plan mode"),
    ("export", "Export the session as a native file"),
    ("export-xml", "Export the session as XML for debugging"),
    ("import", "Import a native session"),
];

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
    /// Compact the active session, with optional instructions.
    Compact(Option<String>),
    /// Run this area's palette command `name` (without the `sessions.`
    /// prefix), typed as a `/` command.
    Command(String),
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

/// What follows a plan review.
#[derive(Clone, Debug)]
pub enum PlanNext {
    Execute(String),
    Revise { prompt: String, permit: Held, step: NodeId },
}

/// A plan permit carried by a message.
#[derive(Clone)]
pub struct Held(Arc<Mutex<Option<Box<dyn PlanPermit>>>>);

impl Held {
    fn new(permit: Box<dyn PlanPermit>) -> Self {
        Self(Arc::new(Mutex::new(Some(permit))))
    }

    fn take(&self) -> Option<Box<dyn PlanPermit>> {
        self.0.locked().take()
    }
}

impl fmt::Debug for Held {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Held")
    }
}

/// A session in app plan mode.
struct Plan {
    /// Keeps the read-only gate on.
    permit: Box<dyn PlanPermit>,
    /// The open plan step; `None` while a plan waits for review.
    step: Option<NodeId>,
}

pub struct State {
    agents: agents::State,
    tree: tree::Tree,
    transfer: transfer::State,
    /// Read-only tool gate of app plan mode.
    gate: Arc<PlanGate>,
    plans: HashMap<String, Plan>,
    /// Rules and advisor state per session whose harness lacks them.
    hooks: Mutex<HashMap<String, TurnHooks>>,
    /// The small model; `None` when omp, which runs it, isn't installed.
    small: Option<OneShot>,
    /// Home directory: native sessions, omp's rules and config.
    home: PathBuf,
    /// Compacted successor id -> the session it replaces.
    compacted: HashMap<String, String>,
    /// Opens links clicked in the transcript.
    opener: Arc<dyn UrlOpener>,
    /// The fork or rewind being started, to show the Session tab once it
    /// is active.
    branching: Option<String>,
}

impl State {
    pub fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
        // Unit tests never make model calls; they install a fake.
        let small = if cfg!(test) {
            None
        } else {
            loa_util::exec::which("omp").map(|omp| OneShot::new(Arc::new(loa_projects::OmpSummarizer::new(omp, ctx.services.cwd.clone()))))
        };
        ctx.composer.set_commands(composer_commands(None));
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

    /// Runs compaction summaries and advisor notes on `model`.
    #[cfg(test)]
    pub(crate) fn set_small_model(&mut self, model: Arc<dyn loa_projects::Summarizer>) {
        self.small = Some(OneShot::new(model));
    }

    /// Reads rules, omp's config and native sessions under `home`.
    #[cfg(test)]
    pub(crate) fn set_home(&mut self, home: PathBuf) {
        self.home = home;
    }

    /// Opens transcript links with `opener`.
    #[cfg(test)]
    pub(crate) fn set_opener(&mut self, opener: Arc<dyn UrlOpener>) {
        self.opener = opener;
    }

    /// Rebuilds from the active session.
    fn reload(&mut self, ctx: &mut Ctx) {
        let empty = Dom::new();
        let dom = ctx.sessions.active().map_or(&empty, |session| session.dom());
        self.agents.reset(dom);
        let _ = self.agents.update(agents::Message::Tick(loa_core::now_ms()));
        ctx.composer.set_mode(mode(dom));
        ctx.composer.set_commands(composer_commands(ctx.sessions.active().map(|session| session.dom())));
        self.tree.refresh(ctx);
        self.restore_plan(ctx);
    }

    /// Takes a reopened session that is in app plan mode back into it.
    fn restore_plan(&mut self, ctx: &mut Ctx) {
        let Some(session) = ctx.sessions.active() else { return };
        let id = session.id().to_string();
        let harness = session.harness_id().unwrap_or_default().to_owned();
        if harness == native::OMP {
            self.gate.enforce_through_approvals(&id);
        }
        if mode(session.dom()) != SessionMode::Plan
            || self.plans.contains_key(&id)
            || capabilities(ctx, &harness).has(Feature::PlanMode)
        {
            return;
        }
        let step = plan_mode::open_step(session.dom());
        match self.gate.permit(&id) {
            Ok(permit) => {
                self.plans.insert(id, Plan { permit, step });
            }
            Err(error) => ctx.notify(format!("Plan mode can't be enforced here: {error}")),
        }
    }

    fn set_mode(&mut self, mode: SessionMode, ctx: &mut Ctx) -> Task<Message> {
        let Some(session) = ctx.sessions.active() else { return Task::none() };
        let id = session.id().to_string();
        let harness = session.harness().cloned();
        let harness_id = session.harness_id().unwrap_or_default().to_owned();
        let current = self::mode(session.dom());
        let busy = session.is_busy();
        let writer = Arc::clone(session.writer());
        let root = session.dom().root().map(|root| root.id);
        if capabilities(ctx, &harness_id).has(Feature::PlanMode) {
            // The harness has its own plan mode.
            let Some(harness) = harness else {
                ctx.notify("The mode changes once the session runs: send a message first");
                ctx.composer.set_mode(current);
                return Task::none();
            };
            return Task::perform(async move { harness.set_mode(mode).await.map_err(|error| error.to_string()) }, Message::Done);
        }
        match mode {
            SessionMode::Plan if !self.plans.contains_key(&id) => {
                if busy {
                    ctx.notify("Plan mode starts between turns: wait for this one to end");
                    ctx.composer.set_mode(SessionMode::Normal);
                    return Task::none();
                }
                if harness_id == native::OMP {
                    self.gate.enforce_through_approvals(&id);
                }
                let entered = self.gate.permit(&id).and_then(|permit| {
                    // `begin` commits through the writer, so it reads a copy:
                    // committing under `with_dom` would deadlock on its lock.
                    let dom = writer.with_dom(Dom::clone);
                    let step = plan_mode::begin(&dom, writer.as_ref())?;
                    Ok((permit, step))
                });
                match entered {
                    Ok((permit, step)) => {
                        self.plans.insert(id, Plan { permit, step: Some(step) });
                        ctx.notify("Plan mode: tools that change files or run commands are blocked until you approve a plan");
                    }
                    Err(error) => {
                        ctx.notify(format!("Can't enter plan mode on {}: {error}", ctx.services.harnesses.label(&harness_id)));
                        ctx.composer.set_mode(SessionMode::Normal);
                    }
                }
            }
            SessionMode::Normal => {
                if self.plans.remove(&id).is_some()
                    && let Some(root) = root
                    && let Err(error) = writer.commit(Draft::set(root, attr::MODE, SessionMode::Normal))
                {
                    ctx.notify(error.to_string());
                }
            }
            SessionMode::Plan => {}
        }
        Task::none()
    }

    /// Branches the session `source` at `target`: a fork when `current` is
    /// `None`, else a rewind from `current`. The branch becomes active.
    fn branch(&mut self, ctx: &mut Ctx, source: &str, target: loa_features::Result<Dom>, current: Option<Dom>) {
        let verb = if current.is_some() { "rewind" } else { "fork" };
        let target = match target {
            Ok(target) => target,
            Err(error) => {
                ctx.notify(format!("Can't {verb} this session: {error}"));
                return;
            }
        };
        let Some(harness) = target.root().and_then(|root| root.str_attr(attr::HARNESS)).map(str::to_owned) else {
            ctx.notify(format!("Can't {verb} a session that never started"));
            return;
        };
        let Some(adapter) = ctx.services.harnesses.runnable(&harness) else {
            ctx.notify(format!("Can't {verb}: {} is unavailable", ctx.services.harnesses.label(&harness)));
            return;
        };
        let title = title_of(current.as_ref().unwrap_or(&target));
        let title = match current {
            Some(_) => format!("{title} (rewound)"),
            None => format!("Fork of {title}"),
        };
        if ctx.sessions.get(source).is_some_and(|session| session.is_busy()) {
            ctx.notify("The earlier branch keeps running; stop it from its own session if it should end");
        }
        let id = new_session_id();
        let library = Arc::clone(&ctx.services.library);
        let target = Arc::new(target);
        let current = current.map(Arc::new);
        let new_id = id.clone();
        let start: SpawnStart = Arc::new(move |config: SessionConfig| {
            let (adapter, library, target, current, title, id) =
                (Arc::clone(&adapter), Arc::clone(&library), Arc::clone(&target), current.clone(), title.clone(), new_id.clone());
            Box::pin(async move {
                let running = match current {
                    None => loa_features::fork(adapter.as_ref(), library.as_ref(), &target, config).await,
                    Some(current) => loa_features::rewind(adapter.as_ref(), library.as_ref(), &current, &target, config)
                        .await
                        .map(|(_, running)| running),
                };
                let running = running.map_err(|error| error.to_string())?;
                name(&library, &id, &title)?;
                Ok(Arc::from(running.harness))
            })
        });
        self.branching = Some(id.clone());
        ctx.request(Request::Spawn(Spawn { id: SessionId::from(id), harness, start, prompt: None }));
    }

    fn fork_active(&mut self, ctx: &mut Ctx) {
        let Some(id) = ctx.sessions.active_id().map(ToString::to_string) else {
            ctx.notify("Open a session to fork it");
            return;
        };
        let target = ctx.services.library.load(&id);
        self.branch(ctx, &id, target, None);
    }

    /// Rewinds session `id` to before the turn holding `node`.
    fn rewind(&mut self, ctx: &mut Ctx, id: &str, node: NodeId) {
        let current = match ctx.services.library.load(id) {
            Ok(current) => current,
            Err(error) => {
                ctx.notify(format!("Can't rewind this session: {error}"));
                return;
            }
        };
        let target = ctx.services.library.before_turn(id, node);
        self.branch(ctx, id, target, Some(current));
    }

    fn compact(&mut self, ctx: &mut Ctx, instructions: Option<String>) -> Task<Message> {
        let Some(session) = ctx.sessions.active() else {
            ctx.notify("Open a session to compact it");
            return Task::none();
        };
        let Some(running) = session.harness().cloned() else {
            ctx.notify("Compaction needs the session running: send a message to resume it first");
            return Task::none();
        };
        if session.is_busy() {
            ctx.notify("Wait for the turn to end before compacting");
            return Task::none();
        }
        let harness = running.harness().to_owned();
        let Some(adapter) = ctx.services.harnesses.registry.get(&harness) else { return Task::none() };
        if adapter.capabilities().has(Feature::Compaction) {
            ctx.notify("Compacting…");
            return Task::perform(async move { running.compact(instructions).await.map_err(|error| error.to_string()) }, Message::Done);
        }
        let Some(small) = self.small.clone() else {
            ctx.notify("Compaction needs a small model call through omp, and omp isn't installed");
            return Task::none();
        };
        let old = session.id().to_string();
        let source = Arc::new(session.writer().with_dom(Dom::clone));
        let writer = Arc::clone(session.writer());
        let title = format!("{} (compacted)", title_of(&source));
        let id = new_session_id();
        self.compacted.insert(id.clone(), old);
        let library = Arc::clone(&ctx.services.library);
        let new_id = id.clone();
        let start: SpawnStart = Arc::new(move |config: SessionConfig| {
            let (adapter, library, small, running, source, writer, instructions, title, id) = (
                Arc::clone(&adapter),
                Arc::clone(&library),
                small.clone(),
                Arc::clone(&running),
                Arc::clone(&source),
                Arc::clone(&writer),
                instructions.clone(),
                title.clone(),
                new_id.clone(),
            );
            Box::pin(async move {
                let request = Compaction {
                    running: running.as_ref(),
                    source: &source,
                    source_sink: writer.as_ref(),
                    next: config,
                    smol_model: SMALL_MODEL,
                    instructions: instructions.as_deref(),
                };
                let compacted = loa_features::compact(adapter.as_ref(), library.as_ref(), &small, request)
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "the harness compacted the session itself".to_owned())?;
                name(&library, &id, &title)?;
                Ok(Arc::from(compacted.harness))
            })
        });
        ctx.notify("Summarizing the session with the small model…");
        ctx.request(Request::Spawn(Spawn { id: SessionId::from(id), harness, start, prompt: None }));
        Task::none()
    }

    fn delete(&mut self, ctx: &mut Ctx, id: &str) -> Task<Message> {
        self.tree.forget(id);
        self.plans.remove(id);
        self.hooks.locked().remove(id);
        let task = if ctx.sessions.get(id).is_some() {
            match ctx.discard_session(id) {
                Some(harness) => {
                    Task::perform(async move { harness.shutdown().await.map_err(|error| error.to_string()) }, Message::Done)
                }
                None => Task::none(),
            }
        } else {
            match ctx.services.library.delete(id) {
                Ok(()) => ctx.shell.remove_session(id),
                Err(error) => ctx.notify(format!("Can't delete session {id}: {error}")),
            }
            Task::none()
        };
        if ctx.shell.sidebar().session(id).is_none() {
            unpin(ctx, id);
        }
        task
    }

    fn role(&mut self, ctx: &mut Ctx, role: String) -> Task<Message> {
        let Some(session) = ctx.sessions.active() else {
            ctx.notify("Open a session to switch its model role");
            return Task::none();
        };
        let Some(harness) = session.harness().cloned() else {
            ctx.notify("Model roles switch a running session: send a message to resume it first");
            return Task::none();
        };
        let Some(root) = session.dom().root().map(|root| root.id) else { return Task::none() };
        let candidates = match model_roles(&self.home) {
            Ok(roles) => match roles.iter().find(|(name, _)| *name == role) {
                Some((_, models)) => models.clone(),
                None => {
                    let names: Vec<&str> = roles.iter().map(|(name, _)| name.as_str()).collect();
                    ctx.notify(if role.is_empty() {
                        format!("Name a model role: /role <{}>", names.join("|"))
                    } else {
                        format!("omp has no model role {role}; its roles: {}", names.join(", "))
                    });
                    return Task::none();
                }
            },
            Err(error) => {
                ctx.notify(error);
                return Task::none();
            }
        };
        let capabilities = capabilities(ctx, harness.harness());
        let writer = Arc::clone(session.writer());
        Task::perform(
            async move {
                loa_features::select_role(harness.as_ref(), &capabilities, writer.as_ref(), root, &role, &candidates)
                    .await
                    .map(|model| (role, model))
                    .map_err(|error| error.to_string())
            },
            Message::RoleSet,
        )
    }

    /// Opens the session behind a job of the Agents tab: a stored session
    /// directly, else the harness's native subagent file imported as a
    /// linked child of the active session.
    fn open_child(&mut self, ctx: &mut Ctx, child: String) {
        if let Some(id) = self.child_session(ctx, child) {
            ctx.request(Request::OpenSession(id));
        }
    }

    /// Opens a subagent's session in its own window, next to its parent.
    fn detach_child(&mut self, ctx: &mut Ctx, child: String) {
        if let Some(id) = self.child_session(ctx, child) {
            ctx.request(Request::DetachSession(id));
        }
    }

    /// The library id of a subagent's session: stored already, or imported
    /// from the harness's session file on this machine.
    fn child_session(&mut self, ctx: &mut Ctx, child: String) -> Option<String> {
        let stored = ctx
            .services
            .library
            .store()
            .list()
            .is_ok_and(|listings| listings.iter().any(|listing| matches!(listing, Listing::Session(info) if info.id == child)));
        if stored {
            return Some(child);
        }
        let path = PathBuf::from(&child);
        if !path.is_file() {
            ctx.notify(format!("The subagent's session {child} isn't on this machine"));
            return None;
        }
        let parent = ctx.sessions.active()?;
        let parent_id = parent.id().to_string();
        let Some(adapter) = parent.harness_id().and_then(|harness| ctx.services.harnesses.registry.get(harness)) else {
            ctx.notify("The parent session's harness isn't supported here");
            return None;
        };
        let job = self.agents.jobs().find(|job| job.session() == Some(child.as_str())).map_or(NodeId::DOCUMENT, agents::Job::node);
        match ctx.services.library.import_child(adapter.as_ref(), &parent_id, job, &path, &ctx.services.node.key) {
            Ok(id) => Some(id),
            Err(error) => {
                ctx.notify(format!("Can't open the subagent's session: {error}"));
                None
            }
        }
    }

    fn transfer(&mut self, ctx: &mut Ctx, message: transfer::Message) {
        match message {
            transfer::Message::Path(path) => self.transfer.path = path,
            transfer::Message::ExportPath(path) => self.transfer.export_path = path,
            transfer::Message::Refresh => self.transfer.refresh(&self.home),
            transfer::Message::Close => ctx.close_screen(),
            transfer::Message::ImportPath => {
                let path = PathBuf::from(self.transfer.path.trim());
                self.import(ctx, &path);
            }
            transfer::Message::Import(path) => self.import(ctx, &path),
            transfer::Message::Export { xml } => {
                let path = Some(self.transfer.export_path.trim()).filter(|path| !path.is_empty()).map(PathBuf::from);
                self.export(ctx, xml, path);
            }
        }
    }

    /// Reads the native session file at `path` into a new session and opens it.
    fn import(&mut self, ctx: &mut Ctx, path: &Path) {
        let result = (|| {
            let harness = native::harness_of(path).ok_or_else(|| format!("{} isn't a session file of a known harness", path.display()))?;
            let adapter = ctx
                .services
                .harnesses
                .registry
                .get(harness)
                .filter(|adapter| adapter.capabilities().import)
                .ok_or_else(|| format!("This app can't import {harness} sessions"))?;
            let id = new_session_id();
            ctx.services.library.import(adapter.as_ref(), path, &id, &ctx.services.node.key).map_err(|error| error.to_string())?;
            Ok::<_, String>(id)
        })();
        match result {
            Ok(id) => {
                self.transfer.status = Some(format!("Imported {}", path.display()));
                ctx.request(Request::OpenSession(id));
            }
            Err(error) => {
                self.transfer.status = Some(format!("Can't import: {error}"));
                ctx.notify(format!("Can't import: {error}"));
            }
        }
    }

    /// Writes the active session to `path` (default: the app's exports
    /// folder) as a native session of its harness or as XML.
    fn export(&mut self, ctx: &mut Ctx, xml: bool, path: Option<PathBuf>) {
        let Some(session) = ctx.sessions.active() else {
            ctx.notify("Open a session to export it");
            return;
        };
        let id = session.id().to_string();
        let harness = session.harness_id().unwrap_or_default().to_owned();
        let extension = if xml { "xml" } else { transfer::extension(&harness) };
        let path = path.unwrap_or_else(|| ctx.services.dirs.data.join("exports").join(format!("{id}.{extension}")));
        let result = (|| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
            }
            if xml {
                return ctx.services.library.export_xml(&id, &path).map_err(|error| error.to_string());
            }
            let adapter = ctx
                .services
                .harnesses
                .registry
                .get(&harness)
                .filter(|adapter| adapter.capabilities().export)
                .ok_or_else(|| format!("{} sessions can't be exported", ctx.services.harnesses.label(&harness)))?;
            ctx.services.library.export(adapter.as_ref(), &id, &path).map_err(|error| error.to_string())
        })();
        let status = match result {
            Ok(()) => format!("Exported to {}", path.display()),
            Err(error) => format!("Can't export: {error}"),
        };
        self.transfer.status = Some(status.clone());
        ctx.notify(status);
    }

    /// Plan mode at the end of a turn: the plan the agent wrote goes up for
    /// review.
    fn submit_plan(&mut self, ctx: &mut Ctx, session: &SessionId, turn: NodeId) {
        let Some(plan) = self.plans.get_mut(session.as_str()) else { return };
        let Some(step) = plan.step else { return };
        let Some(open) = ctx.sessions.get(session.as_str()) else { return };
        let dom = open.dom();
        let (Some(root), Some(text)) = (dom.root().map(|root| root.id), plan_mode::plan_text(dom, turn)) else { return };
        match loa_features::submit_plan(open.writer().as_ref(), root, step, text) {
            Ok(_) => plan.step = None,
            Err(error) => ctx.notify(format!("Can't raise the plan review: {error}")),
        }
    }

    fn answer_plan(&mut self, ctx: &mut Ctx, session: SessionId, decision: NodeId, answer: Answer) -> Task<Message> {
        let Some(open) = ctx.sessions.get(session.as_str()) else { return Task::none() };
        let Some(harness) = open.harness().cloned() else {
            plan_failed(ctx, &session, decision, "The session isn't running: send a message to resume it, then answer the plan");
            return Task::none();
        };
        let Some(plan) = self.plans.remove(session.as_str()) else {
            plan_failed(ctx, &session, decision, "This session isn't in plan mode anymore");
            return Task::none();
        };
        let reason = answer.reason.clone().unwrap_or_default();
        let verdict = match answer.values.first().map(String::as_str) {
            Some(answer::APPROVE) => Verdict::Approve,
            Some(answer::EDIT) => Verdict::Edit(reason),
            _ => Verdict::Reject(reason),
        };
        let dom = open.dom().clone();
        let writer = Arc::clone(open.writer());
        let by = if answer.by.is_empty() { ctx.services.node.key.clone() } else { answer.by };
        let id = session.clone();
        Task::perform(
            async move {
                let outcome = plan_mode::answer_plan(harness.as_ref(), &dom, writer.as_ref(), decision, &by, verdict, plan.permit)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(match outcome {
                    Outcome::Execute { prompt } => PlanNext::Execute(prompt),
                    Outcome::Revise { prompt, permit, step } => PlanNext::Revise { prompt, permit: Held::new(permit), step },
                })
            },
            move |result| Message::PlanAnswered { session: id.clone(), decision, result },
        )
    }

    /// Asks the advisor about a finished turn of a session whose harness has
    /// no advisor of its own.
    fn advise(&self, ctx: &Ctx, session: &SessionId, turn: NodeId) -> Task<Message> {
        let Some(small) = self.small.clone() else { return Task::none() };
        let Some(open) = ctx.sessions.get(session.as_str()) else { return Task::none() };
        if capabilities(ctx, open.harness_id().unwrap_or_default()).has(Feature::AdvisorRules) {
            return Task::none();
        }
        let rules = self.hooks.locked().get(session.as_str()).and_then(|hooks| hooks.rules().map(str::to_owned));
        let dom = open.dom().clone();
        let id = session.clone();
        Task::perform(
            async move {
                Advisor::new(small, SMALL_MODEL).review(&dom, turn, rules.as_deref()).await.map_err(|error| error.to_string())
            },
            move |result| Message::Advised { session: id.clone(), turn, result },
        )
    }

    /// The rules text for a session working in `cwd`.
    fn rules(&self, cwd: &Path) -> Option<String> {
        hooks::rules_context(&hooks::discover_rules(cwd, &self.home))
    }
}

/// A `/` command typed in the composer: its name and its trimmed arguments.
pub fn split_slash(text: &str) -> Option<(&str, &str)> {
    let rest = text.trim().strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    Some((name, args.trim()))
}

/// The message for a `/` command typed in the composer, when it is one of
/// this area's.
pub fn slash(text: &str) -> Option<Message> {
    let (name, args) = split_slash(text)?;
    Some(match name {
        COMPACT => Message::Compact((!args.is_empty()).then(|| args.to_owned())),
        "plan" => Message::Mode(SessionMode::Plan),
        "normal" => Message::Mode(SessionMode::Normal),
        "role" => Message::Command(format!("{ROLE_COMMAND}{args}")),
        "fork" | "export" | "export-xml" | "import" => Message::Command(name.to_owned()),
        _ => return None,
    })
}

/// The composer's `/` commands: the app's, then the harness's own from the
/// latest list its adapter recorded in `dom` (omp's `commands`, ACP's
/// `availableCommands`). A harness command named like an app command is
/// left out: the app's runs.
fn composer_commands(dom: Option<&Dom>) -> Vec<composer::Command> {
    let mut commands: Vec<composer::Command> = APP_COMMANDS
        .iter()
        .map(|(name, description)| composer::Command { name: (*name).to_owned(), description: (*description).to_owned() })
        .collect();
    let listed = dom.and_then(|dom| {
        dom.iter()
            .filter(|node| node.kind == ElementKind::Ext && node.str_attr(attr::TYPE) == Some(chat::COMMANDS_EXT))
            .max_by_key(|node| node.id)
    });
    let Some(listed) = listed else { return commands };
    let harness = listed.str_attr(attr::HARNESS).unwrap_or("harness");
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&listed.text) else { return commands };
    let list = json.get("commands").or_else(|| json.get("availableCommands")).and_then(serde_json::Value::as_array);
    for command in list.into_iter().flatten() {
        let Some(name) = command.get("name").and_then(serde_json::Value::as_str) else { continue };
        if commands.iter().any(|known| known.name == name) {
            continue;
        }
        let description = command.get("description").and_then(serde_json::Value::as_str).unwrap_or_default();
        commands.push(composer::Command { name: name.to_owned(), description: format!("{harness}: {description}") });
    }
    commands
}

/// Drops session `id` from the sidebar's pins and the saved ones.
fn unpin(ctx: &mut Ctx, id: &str) {
    if ctx.shell.sidebar().pins().contains(id) {
        let pins: Vec<String> = ctx.shell.sidebar().pins().iter().filter(|pin| *pin != id).cloned().collect();
        ctx.shell.set_pins(pins);
    }
    ctx.edit_config("pins", |config| config.pinned.remove(id));
}

/// The session's mode, `normal` unless the DOM says otherwise.
fn mode(dom: &Dom) -> SessionMode {
    dom.root().and_then(|root| root.str_attr(attr::MODE)).and_then(SessionMode::parse).unwrap_or(SessionMode::Normal)
}

/// Reopens the plan review card `decision` after its answer failed; the
/// decisions area shows `error`.
fn plan_failed(ctx: &mut Ctx, session: &SessionId, decision: NodeId, error: &str) {
    let reopen = crate::areas::decisions::Message::Answered { session: session.to_string(), decision, result: Err(error.to_owned()) };
    ctx.request(Request::Forward(Box::new(crate::app::Message::Decisions(reopen))));
}

/// The session's title as the sidebar shows it.
fn title_of(dom: &Dom) -> String {
    SessionSummary::from_dom(dom, false).map_or_else(|| "session".to_owned(), |summary| summary.title)
}

fn capabilities(ctx: &Ctx, harness: &str) -> Capabilities {
    ctx.services.harnesses.registry.get(harness).map(|adapter| adapter.capabilities()).unwrap_or_default()
}

/// Titles the new session `id`.
fn name(library: &loa_features::library::Library, id: &str, title: &str) -> Result<(), String> {
    let live = library.live(id).ok_or_else(|| format!("session {id} isn't open"))?;
    let root = live.writer.with_dom(|dom| dom.root().map(|root| root.id)).ok_or_else(|| format!("session {id} has no root"))?;
    live.writer.commit(Draft::set(root, attr::TITLE, title)).map_err(|error| error.to_string())?;
    Ok(())
}

/// omp's model roles (`modelRoles` in `~/.omp/agent/config.yml`) with their
/// candidate models in order.
fn model_roles(home: &Path) -> Result<Vec<(String, Vec<String>)>, String> {
    let path = home.join(".omp/agent/config.yml");
    let config = loa_models::config::OmpConfig::load(&path).map_err(|error| format!("Can't read {}: {error}", path.display()))?;
    let roles = config.model_roles().map_err(|error| format!("Can't read the model roles in {}: {error}", path.display()))?;
    Ok(roles.into_iter().map(|role| (role.role, role.candidates.iter().map(ToString::to_string).collect())).collect())
}

impl Area for State {
    type Message = Message;

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Agents(message) => {
                let (task, event) = self.agents.update(message);
                match event {
                    Some(agents::Event::OpenSession(child)) => self.open_child(ctx, child),
                    Some(agents::Event::DetachSession(child)) => self.detach_child(ctx, child),
                    Some(agents::Event::Cancel { .. }) => {
                        // Harnesses stop jobs only with the turn that runs them.
                        if let Some(harness) = ctx.sessions.active().and_then(|session| session.harness().cloned()) {
                            ctx.notify("Interrupting the turn that runs this job");
                            return Task::batch([
                                task.map(Message::Agents),
                                Task::perform(async move { harness.interrupt().await.map_err(|error| error.to_string()) }, Message::Done),
                            ]);
                        }
                    }
                    None => {}
                }
                task.map(Message::Agents)
            }
            Message::Tree(message) => {
                let (task, event) = self.tree.view.update(message);
                match event {
                    Some(session_tree::Event::Open(id)) => ctx.request(Request::OpenSession(id)),
                    Some(session_tree::Event::Fork { session, turn }) => {
                        let target = ctx.services.library.through_turn(&session, turn);
                        self.branch(ctx, &session, target, None);
                    }
                    Some(session_tree::Event::Rewind { session, turn }) => self.rewind(ctx, &session, turn),
                    None => {}
                }
                task.map(Message::Tree)
            }
            Message::Chat(chat::Event::Rewind(node)) => {
                if let Some(id) = ctx.sessions.active_id().map(ToString::to_string) {
                    self.rewind(ctx, &id, node);
                }
                Task::none()
            }
            Message::Chat(chat::Event::OpenLink(link)) => {
                if !(link.starts_with("https://") || link.starts_with("http://")) {
                    ctx.notify(format!("Only web links open in the browser: {link}"));
                } else if let Err(error) = self.opener.open_url(&link) {
                    ctx.notify(format!("Can't open {link}: {error}"));
                }
                Task::none()
            }
            Message::Mode(mode) => {
                // Also typed as `/plan` or `/normal`: the picker follows.
                ctx.composer.set_mode(mode);
                self.set_mode(mode, ctx)
            }
            Message::Delete(id) => self.delete(ctx, &id),
            Message::Compact(instructions) => self.compact(ctx, instructions),
            Message::Command(name) => self.run_command(AreaId::Sessions.command(&name).as_str(), ctx).unwrap_or_else(Task::none),
            Message::AnswerPlan { session, decision, answer } => self.answer_plan(ctx, session, decision, answer),
            Message::PlanAnswered { session, decision, result } => {
                let prompt = match result {
                    Ok(PlanNext::Execute(prompt)) => prompt,
                    Ok(PlanNext::Revise { prompt, permit, step }) => {
                        if let Some(permit) = permit.take() {
                            self.plans.insert(session.to_string(), Plan { permit, step: Some(step) });
                        }
                        prompt
                    }
                    Err(error) => {
                        plan_failed(ctx, &session, decision, &format!("Can't answer the plan: {error}"));
                        self.restore_plan(ctx);
                        return Task::none();
                    }
                };
                if ctx.sessions.is_active(session.as_str()) {
                    ctx.request(Request::Prompt(Prompt { text: prompt, by: ctx.services.node.key.clone(), ..Prompt::default() }));
                }
                Task::none()
            }
            Message::Advised { session, turn, result } => {
                match result {
                    Ok(Some(note)) => {
                        if let Some(open) = ctx.sessions.get(session.as_str())
                            && let Err(error) = hooks::record_note(open.writer().as_ref(), turn, &note)
                        {
                            ctx.notify(format!("Can't record the advisor's note: {error}"));
                        }
                        if let Some(hooks) = self.hooks.locked().get_mut(session.as_str()) {
                            hooks.advise(note);
                        }
                    }
                    Ok(None) => {}
                    Err(error) => ctx.notify(format!("The advisor failed: {error}")),
                }
                Task::none()
            }
            Message::RoleSet(result) => {
                match result {
                    Ok((role, model)) => ctx.notify(format!("Model role {role}: {model}")),
                    Err(error) => ctx.notify(format!("Can't switch the model role: {error}")),
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

    fn palette_commands(&self, ctx: &Ctx) -> Vec<palette::Command> {
        let mut commands = vec![
            palette::Command::new(AreaId::Sessions.command("import"), "Import native session…", "Session"),
        ];
        if let Some(session) = ctx.sessions.active() {
            let plan = self.plans.contains_key(session.id().as_str()) || mode(session.dom()) == SessionMode::Plan;
            commands.extend([
                palette::Command::new(AreaId::Sessions.command("fork"), "Fork session", "Session"),
                palette::Command::new(AreaId::Sessions.command("compact"), "Compact session", "Session"),
                palette::Command::new(
                    AreaId::Sessions.command("plan"),
                    if plan { "Leave plan mode" } else { "Enter plan mode" },
                    "Session",
                ),
                palette::Command::new(AreaId::Sessions.command("delete"), "Delete session", "Session"),
                palette::Command::new(AreaId::Sessions.command("export"), "Export session as native file", "Session"),
                palette::Command::new(AreaId::Sessions.command("export-xml"), "Export session as XML (debug)", "Session"),
            ]);
            if let Ok(roles) = model_roles(&self.home) {
                commands.extend(roles.into_iter().map(|(role, models)| {
                    let id = AreaId::Sessions.command(&format!("{ROLE_COMMAND}{role}"));
                    let first = models.first().map_or("no model", String::as_str);
                    palette::Command::new(id, format!("Model role: {role} ({first})"), "Model roles")
                }));
            }
        }
        commands
    }

    fn run_command(&mut self, id: &str, ctx: &mut Ctx) -> Option<Task<Message>> {
        let name = id.strip_prefix(AreaId::Sessions.key())?.strip_prefix('.')?;
        if let Some(role) = name.strip_prefix(ROLE_COMMAND) {
            return Some(self.role(ctx, role.to_owned()));
        }
        Some(match name {
            "import" => {
                self.transfer.refresh(&self.home);
                self.transfer.status = None;
                ctx.show_screen(AreaId::Sessions);
                Task::none()
            }
            "fork" => {
                self.fork_active(ctx);
                Task::none()
            }
            "compact" => self.compact(ctx, None),
            "plan" => {
                let active = ctx.sessions.active().map(|session| session.id().to_string());
                let in_plan = active.as_ref().is_some_and(|id| self.plans.contains_key(id));
                let mode = if in_plan { SessionMode::Normal } else { SessionMode::Plan };
                ctx.composer.set_mode(mode);
                self.set_mode(mode, ctx)
            }
            "delete" => match ctx.sessions.active_id().map(ToString::to_string) {
                Some(id) => self.delete(ctx, &id),
                None => Task::none(),
            },
            "export" => {
                self.export(ctx, false, None);
                Task::none()
            }
            "export-xml" => {
                self.export(ctx, true, None);
                Task::none()
            }
            _ => return None,
        })
    }

    fn panel_tabs(&self) -> Vec<PanelTab> {
        vec![PanelTab::new("agents", "Agents", false), PanelTab::new("session", "Session tree", false)]
    }

    fn view_tab<'a>(&'a self, id: &str, _: &'a Ctx) -> Option<Element<'a, Message>> {
        match id {
            "agents" => Some(self.agents.view().map(Message::Agents)),
            "session" => Some(self.tree.view.view().map(Message::Tree)),
            _ => None,
        }
    }

    fn view_screen<'a>(&'a self, ctx: &'a Ctx) -> Option<Element<'a, Message>> {
        let active = ctx.sessions.active().map(|session| (session.id().as_str(), session.harness_id().unwrap_or("unknown")));
        Some(self.transfer.view(active, loa_core::now_ms()).map(Message::Transfer))
    }

    fn contribute(&self, config: &mut SessionConfig, target: &Target<'_>, _: &Ctx) {
        if target.harness == native::OMP {
            // The app runs omp with `--approval-mode always-ask`: it asks
            // before every tool that writes or executes.
            self.gate.enforce_through_approvals(&config.session_id);
        }
        if !target.capabilities.has(Feature::AdvisorRules) {
            let rules = self.rules(&config.cwd);
            let hooks = TurnHooks::start(config, rules);
            self.hooks.locked().insert(config.session_id.clone(), hooks);
        }
    }

    fn before_send(&mut self, session: &SessionId, prompt: &mut Prompt, ctx: &mut Ctx) {
        let Some(open) = ctx.sessions.get(session.as_str()) else { return };
        if !capabilities(ctx, open.harness_id().unwrap_or_default()).has(Feature::AdvisorRules) {
            let cwd = open.dom().root().and_then(|root| root.str_attr(attr::CWD)).map_or_else(|| ctx.services.cwd.clone(), PathBuf::from);
            let rules = self.rules(&cwd);
            let mut hooks = self.hooks.locked();
            let hooks = hooks.entry(session.to_string()).or_insert_with(|| TurnHooks::resumed(rules.clone()));
            prompt.text = hooks.before_turn(rules, &prompt.text);
        }
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
            SessionEvent::Loaded(id) if ctx.sessions.is_active(id.as_str()) => self.reload(ctx),
            SessionEvent::Patch(id, patch) => {
                let active = ctx.sessions.is_active(id.as_str());
                let mut job_started = false;
                if let Some(session) = ctx.sessions.get(id.as_str()).filter(|_| active) {
                    let dom = session.dom();
                    self.agents.apply(dom, patch);
                    if let Op::Insert { kind, .. } = &patch.op {
                        match kind {
                            ElementKind::Job => job_started = true,
                            ElementKind::Ext
                                if dom.get(patch.node).and_then(|node| node.str_attr(attr::TYPE)) == Some(chat::COMMANDS_EXT) =>
                            {
                                ctx.composer.set_commands(composer_commands(Some(dom)));
                            }
                            _ => {}
                        }
                    }
                    if let Op::Set { name, .. } = &patch.op
                        && name == attr::MODE
                        && dom.root().is_some_and(|root| root.id == patch.node)
                    {
                        let mode = mode(dom);
                        ctx.composer.set_mode(mode);
                    }
                }
                if job_started {
                    ctx.reveal_tab("agents");
                }
            }
            SessionEvent::TurnStarted { session, .. } if ctx.sessions.is_active(session.as_str()) => self.tree.refresh(ctx),
            SessionEvent::DecisionWaiting { session, decision } => {
                let Some(open) = ctx.sessions.get(session.as_str()) else { return Task::none() };
                let (Some(node), Some(harness)) = (open.dom().get(*decision), open.harness().cloned()) else { return Task::none() };
                if let Some(answer) = self.gate.verdict(session.as_str(), node) {
                    let decision = *decision;
                    return Task::perform(
                        async move { harness.answer(decision, answer).await.map_err(|error| error.to_string()) },
                        Message::Done,
                    );
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
