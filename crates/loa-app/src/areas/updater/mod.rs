//! Updates of the app, omp and the harnesses: the Updates screen (palette
//! "Updates", and the sidebar's Updates row while something waits for the
//! user), feed checks at startup and every `[updater].check_interval_secs`,
//! staging, gate checks, hold choices, swaps between turns, rollback and the
//! peers that are behind.
//!
//! Swaps never happen mid turn: the area counts running turns from session
//! events and swaps cleared versions once none runs. New sessions of omp and
//! the harnesses start the version the updater made current; running
//! sessions keep theirs.
//!
//! Pins live in the central config (`[updater].pins`); the updater's own
//! state mirrors them. `[updater].github_token` raises GitHub's rate limit
//! and `[updater].trusted_keys` are the keys app releases must be signed
//! with.

use loa_util::time::now_secs;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iced::widget::{container, text};
use iced::{Element, Fill, Subscription, Task};
use loa_contracts::harness::{Prompt, SessionConfig};
use loa_bus::SessionId;
use loa_ui_palette as palette;
use loa_ui_updates as updates;
use loa_updater::peers::{self, PeerInfo, PeerStanding};
use loa_updater::swap::SwapOutcome;
use loa_updater::{InstalledStatus, Overview, ProductKind, TurnGate, Updater, UpdaterConfig, UpdaterError, default_products};
use semver::Version;

use crate::area::{Area, AreaId, Ctx, SessionEvent, Target};

/// Widest the Updates screen grows.
const MAX_WIDTH: f32 = 920.0;

#[derive(Clone, Debug)]
pub enum Message {
    /// Show the Updates screen (palette, sidebar row).
    Open,
    Screen(updates::Message),
    /// A step finished for this product (`None`: every product), with a
    /// notice to show.
    Done(Option<String>, Option<String>),
    /// The check interval passed.
    CheckDue,
}

pub struct State {
    updater: Result<Updater, String>,
    /// Running turns, for swaps between turns only.
    turns: Arc<TurnGate>,
    /// Sessions mid turn.
    running: usize,
    screen: updates::State,
    /// Executables the updater installed and made current, by product id.
    managed: BTreeMap<String, PathBuf>,
    /// This app's version.
    version: Version,
    interval: Duration,
}

impl State {
    /// Opens the updater under `<data>/updates` and checks every feed.
    pub fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let version = Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is semver");
        let root = ctx.services.dirs.data.join("updates");
        let mut problems = Vec::new();
        let settings = match ctx.services.config.load() {
            Ok(config) => config.updater,
            Err(error) => {
                problems.push(format!("Update settings unreadable ({error}); using defaults"));
                loa_config::UpdaterSettings::default()
            }
        };
        let mut config = UpdaterConfig::new(root, default_products(version.clone()));
        config.github_token = settings.github_token.clone().filter(|token| !token.is_empty());
        for key in &settings.trusted_keys {
            match loa_updater::signature::parse_public_key(key) {
                Ok(key) => config.trusted_keys.push(key),
                Err(error) => problems.push(format!("updater.trusted_keys: {error}")),
            }
        }
        let interval = settings.check_interval();
        let updater = Updater::open(config).map_err(|error| format!("Updates are unavailable: {error}"));
        let mut state = Self::with_updater(updater, version, interval);
        if let Ok(updater) = &state.updater {
            let mut pins = BTreeMap::new();
            for (id, pin) in &settings.pins {
                match Version::parse(pin.trim_start_matches('v')) {
                    Ok(version) => {
                        pins.insert(id.clone(), version);
                    }
                    Err(error) => problems.push(format!("updater.pins.{id}: {error}")),
                }
            }
            if let Err(error) = updater.set_pins(pins) {
                problems.push(format!("updater.pins: {error}"));
            }
        }
        if !problems.is_empty() {
            ctx.notify(problems.join("; "));
        }
        state.refresh(ctx);
        let task = state.check_all(ctx);
        (state, task)
    }

    fn with_updater(updater: Result<Updater, String>, version: Version, interval: Duration) -> Self {
        Self {
            updater,
            turns: Arc::new(TurnGate::default()),
            running: 0,
            screen: updates::State::default(),
            managed: BTreeMap::new(),
            version,
            interval,
        }
    }

    /// Re-reads the updater's snapshot into the screen, the sidebar row and
    /// the executables new sessions start.
    fn refresh(&mut self, ctx: &mut Ctx) {
        let Ok(updater) = &self.updater else { return };
        let overview = updater.overview();
        self.managed = overview
            .products
            .iter()
            .filter_map(|product| match &product.installed {
                InstalledStatus::Managed(path) => Some((product.id.clone(), path.clone())),
                _ => None,
            })
            .collect();
        ctx.shell.set_updates(waiting(&overview));
        let peers = self.peers(ctx);
        self.screen.set_data(updates::Data::from_updater(&overview, &peers, self.running > 0, now_secs()));
    }

    /// The peers whose hello this node has seen, against this app.
    fn peers(&self, ctx: &Ctx) -> Vec<PeerStanding> {
        let known: Vec<PeerInfo> = ctx
            .peers
            .iter()
            // Without a hello there is nothing to compare.
            .filter(|peer| !peer.protocols.is_empty())
            .map(|peer| PeerInfo {
                node: peer.key.to_string(),
                name: peer.name.clone(),
                app_version: peer.app_version.as_deref().and_then(|v| Version::parse(v.trim_start_matches('v')).ok()),
                alpns: peer.protocols.clone(),
            })
            .collect();
        peers::classify(&self.version, &loa_node::local_protocols(), &known)
    }

    /// Checks every feed, as the screen's "Check all" does.
    fn check_all(&mut self, ctx: &mut Ctx) -> Task<Message> {
        let (_, event) = self.screen.update(updates::Message::CheckAll);
        event.map_or_else(Task::none, |event| self.run(event, ctx))
    }

    /// Runs one request of the screen on the updater.
    fn run(&mut self, event: updates::Event, ctx: &mut Ctx) -> Task<Message> {
        let updater = match &self.updater {
            Ok(updater) => updater.clone(),
            Err(error) => {
                ctx.notify(error.clone());
                self.screen.finish_all();
                return Task::none();
            }
        };
        let turns = Arc::clone(&self.turns);
        match event {
            updates::Event::CheckAll => Task::perform(
                async move {
                    updater.detect().await;
                    let failed = updater.check_all().await.into_iter().filter(|(_, result)| result.is_err()).count();
                    (failed > 0).then(|| match failed {
                        1 => "1 update feed couldn't be read; see Updates".to_owned(),
                        n => format!("{n} update feeds couldn't be read; see Updates"),
                    })
                },
                |note| Message::Done(None, note),
            ),
            updates::Event::Check(id) => {
                let done = id.clone();
                Task::perform(
                    async move {
                        let _ = updater.check(&id).await;
                    },
                    move |()| Message::Done(Some(done.clone()), None),
                )
            }
            updates::Event::Stage(id) => {
                let done = id.clone();
                Task::perform(
                    async move {
                        let _ = updater.stage(&id).await;
                    },
                    move |()| Message::Done(Some(done.clone()), None),
                )
            }
            updates::Event::Verify(id) => {
                let done = id.clone();
                Task::perform(
                    async move {
                        let _ = updater.verify(&id).await;
                    },
                    move |()| Message::Done(Some(done.clone()), None),
                )
            }
            updates::Event::Gate(id) => {
                let done = id.clone();
                Task::perform(
                    async move {
                        let report = updater.gate(&id).await.ok()?;
                        if report.is_clean() {
                            // Cleared: swap now unless a turn runs.
                            swap_note(&updater, &id, &updater.swap(&id, &*turns))
                        } else {
                            let harmful = report.harmful().count();
                            Some(format!(
                                "{} {} held: {harmful} gate check(s) found harmful changes",
                                label(&updater, &id),
                                report.to
                            ))
                        }
                    },
                    move |note| Message::Done(Some(done.clone()), note),
                )
            }
            updates::Event::Choose(id, choice) => {
                let note = match updater.choose(&id, choice.into()) {
                    Ok(Some(_)) => swap_note(&updater, &id, &updater.swap(&id, &*turns)),
                    Ok(None) | Err(_) => None,
                };
                self.save_pins(&updater, ctx);
                Task::done(Message::Done(Some(id), note))
            }
            updates::Event::Unpin(id) => {
                let _ = updater.unpin(&id);
                self.save_pins(&updater, ctx);
                Task::done(Message::Done(Some(id), None))
            }
            updates::Event::Swap(id) => {
                let note = swap_note(&updater, &id, &updater.swap(&id, &*turns));
                Task::done(Message::Done(Some(id), note))
            }
            updates::Event::Rollback(id) => {
                let note = swap_note(&updater, &id, &updater.rollback(&id, &*turns));
                Task::done(Message::Done(Some(id), note))
            }
        }
    }

    /// Writes the updater's pins to `[updater].pins` of the central config.
    fn save_pins(&self, updater: &Updater, ctx: &mut Ctx) {
        let pins: BTreeMap<String, String> = updater
            .overview()
            .products
            .into_iter()
            .filter_map(|product| product.pin.map(|pin| (product.id, pin.to_string())))
            .collect();
        ctx.edit_config("pins", |config| config.updater.pins = pins);
    }

    /// Records how many sessions are mid turn; once none is, swaps in the
    /// versions cleared meanwhile.
    fn set_running(&mut self, running: usize, ctx: &mut Ctx) {
        if running == self.running {
            return;
        }
        self.running = running;
        self.turns.set(running);
        if running == 0
            && let Ok(updater) = &self.updater
        {
            let notes: Vec<String> = updater
                .apply_pending(&*self.turns)
                .into_iter()
                .filter_map(|(id, outcome)| swap_note(updater, &id, &outcome))
                .collect();
            if !notes.is_empty() {
                ctx.notify(notes.join("; "));
            }
        }
        self.refresh(ctx);
    }

    /// The test updater in place of the real one.
    #[cfg(test)]
    pub(crate) fn replace_updater(&mut self, updater: Updater, ctx: &mut Ctx) {
        // The startup check of the real updater never runs in tests.
        self.screen = updates::State::default();
        self.updater = Ok(updater);
        self.refresh(ctx);
    }

    #[cfg(test)]
    pub(crate) fn screen(&self) -> &updates::State {
        &self.screen
    }

    #[cfg(test)]
    pub(crate) fn turn_running(&self) -> bool {
        use loa_updater::swap::Activity;
        self.turns.turn_running()
    }
}

impl Area for State {
    type Message = Message;

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Open => {
                ctx.show_screen(AreaId::Updater);
                self.refresh(ctx);
                Task::none()
            }
            Message::Screen(message) => {
                let (task, event) = self.screen.update(message);
                let run = event.map_or_else(Task::none, |event| self.run(event, ctx));
                Task::batch([task.map(Message::Screen), run])
            }
            Message::Done(id, note) => {
                match id {
                    Some(id) => self.screen.finish(&id),
                    None => self.screen.finish_all(),
                }
                self.refresh(ctx);
                if let Some(note) = note {
                    ctx.notify(note);
                }
                Task::none()
            }
            Message::CheckDue => self.check_all(ctx),
        }
    }

    fn palette_commands(&self, _: &Ctx) -> Vec<palette::Command> {
        vec![
            palette::Command::new(AreaId::Updater.command("open"), "Updates", "Commands"),
            palette::Command::new(AreaId::Updater.command("check"), "Check for updates", "Commands"),
        ]
    }

    fn run_command(&mut self, id: &str, ctx: &mut Ctx) -> Option<Task<Message>> {
        let name = id.strip_prefix(AreaId::Updater.key())?.strip_prefix('.')?;
        match name {
            "open" => Some(self.update(Message::Open, ctx)),
            "check" => {
                let open = self.update(Message::Open, ctx);
                Some(Task::batch([open, self.check_all(ctx)]))
            }
            _ => None,
        }
    }

    fn view_screen<'a>(&'a self, _: &'a Ctx) -> Option<Element<'a, Message>> {
        let body: Element<'a, Message> = match &self.updater {
            Ok(_) => self.screen.view().map(Message::Screen),
            Err(error) => text(error.as_str()).size(14).into(),
        };
        Some(container(container(body).max_width(MAX_WIDTH)).center_x(Fill).padding([16, 24]).into())
    }

    fn contribute(&self, config: &mut SessionConfig, target: &Target<'_>, _: &Ctx) {
        // The updater names Claude Code's product `claude`.
        let product = if target.harness == loa_harness::claude::HARNESS { "claude" } else { target.harness };
        if let Some(path) = self.managed.get(product) {
            config.executable = Some(path.clone());
        }
    }

    fn before_send(&mut self, session: &SessionId, _: &mut Prompt, ctx: &mut Ctx) {
        // The turn starts now; a swap must not slip in before its patch.
        let others = ctx.sessions.iter().filter(|open| open.is_busy() && open.id() != session).count();
        self.set_running(others + 1, ctx);
    }

    fn on_session_event(&mut self, event: &SessionEvent, ctx: &mut Ctx) -> Task<Message> {
        if matches!(
            event,
            SessionEvent::TurnStarted { .. } | SessionEvent::TurnEnded { .. } | SessionEvent::Closed(_) | SessionEvent::Loaded(_)
        ) {
            let running = ctx.sessions.iter().filter(|open| open.is_busy()).count();
            self.set_running(running, ctx);
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        iced::time::every(self.interval).map(|_| Message::CheckDue)
    }
}

/// Installed products with something for the user: an offer, a held or
/// pending update, or a restart.
fn waiting(overview: &Overview) -> usize {
    overview
        .products
        .iter()
        .filter(|p| p.current.is_some())
        .filter(|p| p.offer.is_some() || p.held.is_some() || p.pending_swap.is_some() || p.restart_required)
        .count()
}

fn label(updater: &Updater, id: &str) -> String {
    updater.config().products.iter().find(|spec| spec.product.id == id).map_or_else(|| id.to_owned(), |spec| spec.label.clone())
}

/// The notice for a swap or rollback of `id`; errors show on the product
/// instead.
fn swap_note(updater: &Updater, id: &str, outcome: &Result<SwapOutcome, UpdaterError>) -> Option<String> {
    let name = label(updater, id);
    let app = updater.config().products.iter().any(|spec| spec.product.id == id && spec.product.kind == ProductKind::App);
    match outcome {
        Ok(SwapOutcome::Deferred) => Some(format!("{name} swaps in once the running turn ends")),
        Ok(SwapOutcome::AlreadyActive) | Err(_) => None,
        Ok(SwapOutcome::Swapped { to, .. }) if app => Some(format!("{name} {to} is installed; restart Loa to use it")),
        Ok(SwapOutcome::Swapped { to, .. }) => Some(format!("{name} is now {to}; new sessions use it")),
    }
}

#[cfg(test)]
mod tests;
