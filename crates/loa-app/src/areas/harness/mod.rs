//! Harness switching and availability: the composer's harness picker, the
//! palette's "Switch harness" commands and the Harnesses tab, which shows
//! what each adapter's probe found and lets the user point a harness at
//! another executable.

use std::collections::HashMap;
use std::path::PathBuf;

use iced::widget::{column, row, scrollable, space, text_input};
use iced::{Alignment, Element, Fill, Task};
use loa_config::HarnessSettings;
use loa_contracts::harness::{Availability, AvailabilityStatus};
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Tone};
use loa_ui_kit::theme::{SpaceRole, TextRole, sp};
use loa_ui_kit::widgets::{card, label, section_header, text_button};

use crate::area::{perform, Area, AreaCommand, AreaMessage, Ctx, Listed, PanelTab, Request, SessionEvent};
use crate::services::Harness;

#[cfg(test)]
pub(crate) mod tests;

/// Id of the Harnesses panel tab.
pub const TAB: &str = "harnesses";
/// Palette command id that probes every harness again; the others switch to
/// the harness their id names.
const RECHECK: &str = "harness.recheck";

#[derive(Clone, Debug)]
pub enum Message {
    /// Route new sessions to the harness with this id.
    Pick(String),
    /// A discarded session's harness shut down.
    Closed(Result<(), String>),
    /// Probe every harness again.
    Recheck,
    /// Harness `id`'s probe number `probe` answered.
    Probed { id: String, probe: u64, availability: Availability },
    /// The executable field of harness `id` changed.
    EditExecutable(String, String),
    /// Run harness `id` on its executable field from now on; empty restores
    /// the default.
    SaveExecutable(String),
}

pub struct State {
    /// Number of the latest probe per harness; older answers are dropped.
    probes: HashMap<String, u64>,
    /// Harnesses whose latest probe hasn't answered.
    probing: Vec<String>,
    next_probe: u64,
    /// The executable fields of the Harnesses tab.
    drafts: HashMap<String, String>,
    /// `[harness] default`: the harness new sessions start on.
    default: Option<String>,
}

impl State {
    fn apply(&mut self, settings: &HarnessSettings, ctx: &mut Ctx) {
        let services = &mut ctx.services;
        for (id, executable) in &settings.executables {
            if services.harnesses.set_executable(id, Some(executable.clone()), &services.node.key, &services.storage.dirs.omp) {
                self.drafts.insert(id.clone(), executable.display().to_string());
            }
        }
        self.default = Some(settings.default.clone());
        if ctx.sessions.active().is_none() && services.harnesses.get(&settings.default).is_some_and(|harness| harness.option.available) {
            ctx.sessions.harness = Some(settings.default.clone());
        }
        ctx.composer.set_harnesses(ctx.services.harnesses.options(), ctx.sessions.harness.as_deref());
    }

    /// Probes every registered harness.
    fn probe_all(&mut self, ctx: &Ctx) -> Task<Message> {
        let ids: Vec<_> = ctx.services.harnesses.list.iter().map(|harness| harness.option.id.clone()).collect();
        Task::batch(ids.into_iter().map(|id| self.probe(id, ctx)))
    }

    /// Runs harness `id`'s availability probe in the background.
    fn probe(&mut self, id: String, ctx: &Ctx) -> Task<Message> {
        let Some(adapter) = ctx.services.harnesses.registry.get(&id) else { return Task::none() };
        self.next_probe += 1;
        let probe = self.next_probe;
        self.probes.insert(id.clone(), probe);
        if !self.probing.contains(&id) {
            self.probing.push(id.clone());
        }
        Task::perform(async move { adapter.availability().await }, move |availability| Message::Probed { id, probe, availability })
    }

    /// Records a probe's answer and refreshes the picker.
    fn probed(&mut self, id: &str, probe: u64, availability: Availability, ctx: &mut Ctx) {
        if self.probes.get(id) != Some(&probe) {
            return;
        }
        self.probing.retain(|probing| probing != id);
        ctx.services.harnesses.set_availability(id, availability);
        let harnesses = &ctx.services.harnesses;
        let runnable = |id: &str| harnesses.get(id).is_some_and(|harness| harness.option.available);
        if ctx.sessions.active().is_none() && ctx.sessions.starting().is_none() && !ctx.sessions.harness.as_deref().is_some_and(runnable) {
            ctx.sessions.harness = self.default.clone().filter(|default| runnable(default)).or_else(|| harnesses.default_id());
        }
        ctx.composer.set_harnesses(ctx.services.harnesses.options(), ctx.sessions.harness.as_deref());
    }

    /// Routes new sessions to harness `id` and makes it the default.
    fn switch(&mut self, id: String, ctx: &mut Ctx) -> Task<Message> {
        let harnesses = &ctx.services.harnesses;
        if !harnesses.get(&id).is_some_and(|harness| harness.option.available) {
            let label = harnesses.label(&id).to_owned();
            ctx.notify(format!("{label} is unavailable"));
            return Task::none();
        }
        if ctx.sessions.starting().is_some() {
            ctx.notify("A session is already starting");
            ctx.show_harness();
            return Task::none();
        }
        ctx.sessions.harness = Some(id.clone());
        ctx.composer.set_harnesses(ctx.services.harnesses.options(), Some(&id));
        if self.default.as_deref() != Some(id.as_str())
            && ctx.edit_config("harness settings", |config| config.harness.default.clone_from(&id)).is_some()
        {
            self.default = Some(id.clone());
        }
        let Some(session) = ctx.sessions.active() else {
            ctx.request(Request::NewSession);
            return Task::none();
        };
        if session.harness_id() == Some(id.as_str()) {
            return Task::none();
        }
        let empty = !session.is_busy() && !session.has_turns();
        let session = session.id().clone();
        let close = match empty.then(|| ctx.discard_session(session.as_str())).flatten() {
            Some(harness) => perform(async move { harness.shutdown().await }, Message::Closed),
            None => Task::none(),
        };
        ctx.request(Request::NewSession);
        close
    }

    /// Saves harness `id`'s executable field to the config, registers the
    /// harness on it and probes it.
    fn save_executable(&mut self, id: String, ctx: &mut Ctx) -> Task<Message> {
        let draft = self.drafts.get(&id).map(|draft| draft.trim()).unwrap_or_default();
        let executable = (!draft.is_empty()).then(|| PathBuf::from(draft));
        let saved = ctx.edit_config("harness settings", |config| match &executable {
            Some(executable) => drop(config.harness.executables.insert(id.clone(), executable.clone())),
            None => drop(config.harness.executables.remove(&id)),
        });
        if saved.is_none() {
            return Task::none();
        }
        let services = &mut ctx.services;
        if !services.harnesses.set_executable(&id, executable, &services.node.key, &services.storage.dirs.omp) {
            return Task::none();
        }
        ctx.composer.set_harnesses(ctx.services.harnesses.options(), ctx.sessions.harness.as_deref());
        self.probe(id, ctx)
    }

    fn view<'a>(&'a self, ctx: &'a Ctx) -> Element<'a, Message> {
        let checking = !self.probing.is_empty();
        let recheck = text_button(if checking { "Checking…" } else { "Recheck" }, ButtonKind::Secondary, (!checking).then_some(Message::Recheck));
        let selected = ctx.sessions.harness.as_deref();
        let rows = ctx.services.harnesses.list.iter().map(|harness| self.harness_row(harness, selected));
        let body = column![section_header("Harnesses", [recheck]), column(rows).spacing(sp(SpaceRole::S))];
        scrollable(body.spacing(sp(SpaceRole::M)).padding(sp(SpaceRole::M))).height(Fill).into()
    }

    fn harness_row<'a>(&'a self, harness: &'a Harness, selected: Option<&str>) -> Element<'a, Message> {
        let id = harness.option.id.as_str();
        let probing = self.probing.iter().any(|probing| probing == id);
        let status = harness.availability.as_ref().filter(|_| !probing).map(|availability| availability.status);
        let (icon, tone) = match status {
            Some(AvailabilityStatus::Ready) => (Icon::Check, Tone::Ok),
            None | Some(AvailabilityStatus::Unknown) => (Icon::CircleDot, Tone::Muted),
            Some(_) => (Icon::TriangleAlert, Tone::Danger),
        };
        let action = if selected == Some(id) {
            Some(label("New sessions", TextRole::Label, Tone::Accent).into())
        } else {
            harness.option.available.then(|| text_button("Use", ButtonKind::Secondary, Some(Message::Pick(id.to_owned()))))
        };
        let title = row![tinted(icon, TextRole::Body, tone), label(harness.option.label.as_str(), TextRole::Body, Tone::Normal), label(status_word(status), TextRole::Label, tone), space::horizontal()];
        let note = |s: String| label(s, TextRole::Label, Tone::Muted);
        let version = harness.availability.as_ref().and_then(|availability| availability.version.as_deref());
        let detail = harness.option.detail.as_deref().filter(|detail| Some(*detail) != version).map(|detail| note(detail.to_owned()));
        let body = column![title.push(action).spacing(sp(SpaceRole::S)).align_y(Alignment::Center)].push(detail).push(version.map(|version| note(format!("Version {version}"))));
        let body = if harness.traits.server_url {
            body.push(note(format!("Server {}", harness.program)))
        } else {
            let executable = harness.executable.as_ref().map(|executable| label(executable.display().to_string(), TextRole::Mono, Tone::Muted));
            let draft = self.drafts.get(id).map_or("", String::as_str);
            let field = text_input(harness.program, draft)
                .padding(sp(SpaceRole::Xs))
                .style(style::input)
                .on_input(move |value| Message::EditExecutable(id.to_owned(), value))
                .on_submit(Message::SaveExecutable(id.to_owned()));
            let configured = harness.configured.as_ref().map(|path| path.display().to_string());
            let changed = configured.as_deref().unwrap_or("") != draft.trim();
            let save = text_button("Save", ButtonKind::Secondary, changed.then(|| Message::SaveExecutable(id.to_owned())));
            body.push(executable).push(row![field, save].spacing(sp(SpaceRole::Xs)).align_y(Alignment::Center))
        };
        card(body.spacing(sp(SpaceRole::Xs))).width(Fill).into()
    }
}

/// What the Harnesses tab says about a probe result; `None` while probing.
fn status_word(status: Option<AvailabilityStatus>) -> &'static str {
    match status {
        None => "Checking…",
        Some(AvailabilityStatus::Ready) => "Ready",
        Some(AvailabilityStatus::NotInstalled) => "Not installed",
        Some(AvailabilityStatus::Unreachable) => "Not running",
        Some(AvailabilityStatus::Incompatible) => "Incompatible",
        Some(AvailabilityStatus::Failed) => "Failed",
        Some(AvailabilityStatus::Unknown) => "Unknown",
    }
}

impl AreaMessage for Message {
    type Area = State;
}

impl Area for State {
    const ID: &'static str = "harness";
    const COMMANDS: &'static [AreaCommand<Self>] = &[
        AreaCommand::<Self>::new("harness.", "", "Harness", |state, id, ctx| state.switch(id.to_owned(), ctx)).listed(Listed::Each(|_, ctx| {
            let available = ctx.services.harnesses.list.iter().filter(|harness| harness.option.available);
            available.map(|harness| (harness.option.id.clone(), format!("Switch harness: {}", harness.option.label))).collect()
        })),
        AreaCommand::<Self>::new(RECHECK, "Recheck harness availability", "Harness", |state, _, ctx| {
            ctx.open_tab(TAB);
            state.probe_all(ctx)
        }),
    ];

    type Message = Message;

    /// Applies `[harness]` from the central config (executables, then the
    /// default harness) and probes every harness.
    fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let mut state = Self { probes: HashMap::new(), probing: Vec::new(), next_probe: 0, drafts: HashMap::new(), default: None };
        if let Some(config) = ctx.read_config("harness settings") {
            state.apply(&config.harness, ctx);
        }
        let task = state.probe_all(ctx);
        (state, task)
    }

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Pick(id) => self.switch(id, ctx),
            Message::Closed(result) => {
                if let Err(error) = result {
                    ctx.notify(error);
                }
                Task::none()
            }
            Message::Recheck => self.probe_all(ctx),
            Message::Probed { id, probe, availability } => {
                self.probed(&id, probe, availability, ctx);
                Task::none()
            }
            Message::EditExecutable(id, value) => {
                self.drafts.insert(id, value);
                Task::none()
            }
            Message::SaveExecutable(id) => self.save_executable(id, ctx),
        }
    }

    fn panel_tabs(&self) -> Vec<PanelTab> {
        vec![PanelTab::new(TAB, "Harnesses", false)]
    }

    fn view_tab<'a>(&'a self, id: &str, ctx: &'a Ctx) -> Option<Element<'a, Message>> {
        (id == TAB).then(|| self.view(ctx))
    }

    fn on_session_event(&mut self, event: &SessionEvent, ctx: &mut Ctx) -> Task<Message> {
        if let SessionEvent::Activated(Some(_)) = event {
            ctx.show_harness();
        }
        Task::none()
    }
}
