//! Branching a session into a new one: fork, rewind and compaction share one spawn strategy.

use std::sync::Arc;

use iced::Task;
use loa_bus::{SessionId, SessionWriter};
use loa_contracts::harness::{Adapter, Feature, HarnessSession, SessionConfig};
use loa_core::{Dom, Draft, NodeId, PatchSink, attr};
use loa_harness::features::library::Library;
use loa_harness::features::{self, Compaction};

use super::{Message, SMALL_MODEL, State, title_of};
use crate::area::{Ctx, Request, Spawn, SpawnStart, perform};
use crate::session::new_session_id;

/// How a new session branches off its source.
pub(super) enum Branch {
    /// A copy of the source up to `target`.
    Fork(Dom),
    /// The source `current` taken back to `target`.
    Rewind { current: Dom, target: Dom },
    /// A summary of `source` written by the small model, continued on `running`'s harness.
    Compact { running: Arc<dyn HarnessSession>, source: Dom, writer: Arc<SessionWriter>, small: Arc<dyn loa_projects::Summarizers>, instructions: Option<String> },
}

impl Branch {
    fn verb(&self) -> &'static str {
        match self {
            Branch::Fork(_) => "fork",
            Branch::Rewind { .. } => "rewind",
            Branch::Compact { .. } => "compact",
        }
    }

    fn title(&self) -> String {
        match self {
            Branch::Fork(target) => format!("Fork of {}", title_of(target)),
            Branch::Rewind { current, .. } => format!("{} (rewound)", title_of(current)),
            Branch::Compact { source, .. } => format!("{} (compacted)", title_of(source)),
        }
    }

    /// The harness the branch runs on.
    fn harness(&self) -> Option<String> {
        match self {
            Branch::Fork(target) | Branch::Rewind { target, .. } => target.root()?.str_attr(attr::HARNESS).map(str::to_owned),
            Branch::Compact { running, .. } => Some(running.harness().to_owned()),
        }
    }

    /// Creates the new session in `library` and starts it with `config`.
    async fn run(&self, adapter: &dyn Adapter, library: &Library, config: SessionConfig) -> Result<Box<dyn HarnessSession>, String> {
        let text = |error: features::Error| error.to_string();
        match self {
            Branch::Fork(target) => features::fork(adapter, library, target, config).await.map_err(text),
            Branch::Rewind { current, target } => features::rewind(adapter, library, current, target, config).await.map_err(text),
            Branch::Compact { running, source, writer, small, instructions } => {
                let request = Compaction { running: running.as_ref(), source, source_sink: writer.as_ref(), next: config, smol_model: SMALL_MODEL, instructions: instructions.as_deref() };
                let compacted = features::compact(adapter, library, small.on(Some(SMALL_MODEL)).0.as_ref(), request).await.map_err(text)?;
                compacted.ok_or_else(|| "the harness compacted the session itself".to_owned())
            }
        }
    }
}

impl State {
    /// Starts the session `branch` makes of session `source`; it becomes the active one.
    pub(super) fn spawn_branch(&mut self, ctx: &mut Ctx, source: &str, branch: Branch) {
        let verb = branch.verb();
        let Some(harness) = branch.harness() else {
            ctx.notify(format!("Can't {verb} a session that never started"));
            return;
        };
        let Some(adapter) = ctx.services.harnesses.runnable(&harness) else {
            ctx.notify(format!("Can't {verb}: {} is unavailable", ctx.services.harnesses.label(&harness)));
            return;
        };
        let id = new_session_id();
        if let Branch::Compact { .. } = branch {
            self.compacted.insert(id.clone(), source.to_owned());
            ctx.notify("Summarizing the session with the small model…");
        } else {
            if ctx.sessions.get(source).is_some_and(|session| session.is_busy()) {
                ctx.notify("The earlier branch keeps running; stop it from its own session if it should end");
            }
            self.branching = Some(id.clone());
        }
        let (title, library, branch, new_id) = (branch.title(), Arc::clone(&ctx.services.storage.library), Arc::new(branch), id.clone());
        let start: SpawnStart = Arc::new(move |config: SessionConfig| {
            let (adapter, library, branch, title, id) = (Arc::clone(&adapter), Arc::clone(&library), Arc::clone(&branch), title.clone(), new_id.clone());
            Box::pin(async move {
                let running = branch.run(adapter.as_ref(), &library, config).await?;
                name(&library, &id, &title)?;
                Ok(Arc::from(running))
            })
        });
        ctx.request(Request::Spawn(Spawn { id: SessionId::from(id), harness, start, prompt: None }));
    }

    /// Forks the active session as it is now.
    pub(super) fn fork_active(&mut self, ctx: &mut Ctx) {
        let Some(id) = ctx.sessions.active_id().map(ToString::to_string) else {
            ctx.notify("Open a session to fork it");
            return;
        };
        match ctx.services.storage.library.load(&id) {
            Ok(target) => self.spawn_branch(ctx, &id, Branch::Fork(target)),
            Err(error) => ctx.notify(format!("Can't fork this session: {error}")),
        }
    }

    /// Forks session `id` through turn `turn`.
    pub(super) fn fork_at(&mut self, ctx: &mut Ctx, id: &str, turn: NodeId) {
        match ctx.services.storage.library.through_turn(id, turn) {
            Ok(target) => self.spawn_branch(ctx, id, Branch::Fork(target)),
            Err(error) => ctx.notify(format!("Can't fork this session: {error}")),
        }
    }

    /// Rewinds session `id` to before the turn holding `node`.
    pub(super) fn rewind(&mut self, ctx: &mut Ctx, id: &str, node: NodeId) {
        let library = &ctx.services.storage.library;
        match library.load(id).and_then(|current| Ok(Branch::Rewind { target: library.before_turn(id, node)?, current })) {
            Ok(branch) => self.spawn_branch(ctx, id, branch),
            Err(error) => ctx.notify(format!("Can't rewind this session: {error}")),
        }
    }

    /// Compacts the active session: natively when its harness can, else into a summarized new session.
    pub(super) fn compact(&mut self, ctx: &mut Ctx, instructions: &str) -> Task<Message> {
        let instructions = (!instructions.is_empty()).then(|| instructions.to_owned());
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
        if super::capabilities(ctx, running.harness()).has(Feature::Compaction) {
            ctx.notify("Compacting…");
            return perform(async move { running.compact(instructions).await }, Message::Done);
        }
        let Some(small) = self.small.clone() else {
            ctx.notify("Compaction needs a small model call through omp, and omp isn't installed");
            return Task::none();
        };
        let (source, writer, id) = (session.writer().with_dom(Dom::clone), Arc::clone(session.writer()), session.id().to_string());
        self.spawn_branch(ctx, &id, Branch::Compact { running, source, writer, small, instructions });
        Task::none()
    }
}

/// Titles the new session `id`.
fn name(library: &Library, id: &str, title: &str) -> Result<(), String> {
    let live = library.live(id).ok_or_else(|| format!("session {id} isn't open"))?;
    let root = live.writer.with_dom(|dom| dom.root().map(|root| root.id)).ok_or_else(|| format!("session {id} has no root"))?;
    live.writer.commit(Draft::set(root, attr::TITLE, title)).map(drop).map_err(|error| error.to_string())
}
