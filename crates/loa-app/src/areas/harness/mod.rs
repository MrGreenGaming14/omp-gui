//! Harness switching and availability: the composer's harness picker, the
//! palette's "Switch harness" commands and the Harnesses tab, which shows
//! what each adapter's probe found and lets the user point a harness at
//! another executable. The choice of harness and the executables are the
//! `[harness]` table of the central config.

use std::collections::HashMap;
use std::path::PathBuf;

use iced::widget::{button, column, container, row, scrollable, space, svg, text, text_input};
use iced::{Element, Font, Length, Task, Theme};
use loa_contracts::harness::{Availability, AvailabilityStatus};
use loa_config::HarnessSettings;
use loa_ui_kit::icons::Icon;
use loa_ui_shell::palette;
use loa_ui_shell::theme::style;

use crate::area::{Area, AreaId, Ctx, PanelTab, Request, SessionEvent};
use crate::services::{Harness, OPENCODE_URL};

#[cfg(test)]
pub(crate) mod tests;

/// Id of the Harnesses panel tab.
pub const TAB: &str = "harnesses";
/// Palette command name that probes every harness again.
const RECHECK: &str = "recheck";

/// The tab's text sizes and colors, from the theme like the other panel tabs.
mod panel_style {
    use iced::Theme;
    use iced::widget::{button, text};
    use loa_ui_shell::theme::style;

    /// Size of row details.
    pub const TEXT_SIZE: f32 = 13.0;
    /// Size of headings and harness names, matching the sidebar.
    pub const LIST_TEXT_SIZE: f32 = 14.0;

    pub fn muted(theme: &Theme) -> text::Style {
        style::muted(theme)
    }

    pub fn success_text(theme: &Theme) -> text::Style {
        text::Style { color: Some(theme.extended_palette().success.base.color) }
    }

    pub fn danger_text(theme: &Theme) -> text::Style {
        text::Style { color: Some(theme.extended_palette().danger.base.color) }
    }

    pub fn primary_text(theme: &Theme) -> text::Style {
        text::Style { color: Some(theme.extended_palette().primary.base.color) }
    }

    /// A small outlined action inside a row.
    pub fn action_button(theme: &Theme, status: button::Status) -> button::Style {
        button::Style { border: iced::Border { color: style::border_color(theme), width: 1.0, radius: style::RADIUS.into() }, ..style::row(theme, status) }
    }
}

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
    /// Applies `[harness]` from the central config (executables, then the
    /// default harness) and probes every harness.
    pub fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let mut state = Self { probes: HashMap::new(), probing: Vec::new(), next_probe: 0, drafts: HashMap::new(), default: None };
        if let Some(config) = ctx.read_config("harness settings") {
            state.apply(&config.harness, ctx);
        }
        let task = state.probe_all(ctx);
        (state, task)
    }

    fn apply(&mut self, settings: &HarnessSettings, ctx: &mut Ctx) {
        let services = &mut ctx.services;
        for (id, executable) in &settings.executables {
            if services.harnesses.set_executable(id, Some(executable.clone()), &services.node.key, &services.dirs.omp) {
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

    /// Records a probe's answer and refreshes the picker. When the harness
    /// new sessions would start on can't run, the configured default or the
    /// first runnable one takes its place.
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

    /// Routes new sessions to harness `id` and makes it the default. An
    /// active session without turns is restarted on it; one with turns
    /// stays in Recents beside a new one.
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
            Some(harness) => Task::perform(async move { harness.shutdown().await.map_err(|error| error.to_string()) }, Message::Closed),
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
        if !services.harnesses.set_executable(&id, executable, &services.node.key, &services.dirs.omp) {
            return Task::none();
        }
        ctx.composer.set_harnesses(ctx.services.harnesses.options(), ctx.sessions.harness.as_deref());
        self.probe(id, ctx)
    }

    fn view<'a>(&'a self, ctx: &'a Ctx) -> Element<'a, Message> {
        let checking = !self.probing.is_empty();
        let recheck = button(text(if checking { "Checking…" } else { "Recheck" }).size(panel_style::TEXT_SIZE))
            .padding([2, 8])
            .style(panel_style::action_button)
            .on_press_maybe((!checking).then_some(Message::Recheck));
        let header = row![text("Harnesses").size(panel_style::LIST_TEXT_SIZE), space::horizontal(), recheck].align_y(iced::Center);
        let selected = ctx.sessions.harness.as_deref();
        let rows = ctx.services.harnesses.list.iter().map(|harness| self.harness_row(harness, selected));
        let list = column(rows).spacing(8);
        scrollable(column![header, list].spacing(10).padding(12)).height(Length::Fill).into()
    }

    fn harness_row<'a>(&'a self, harness: &'a Harness, selected: Option<&str>) -> Element<'a, Message> {
        let id = harness.option.id.as_str();
        let probing = self.probing.iter().any(|probing| probing == id);
        let status = harness.availability.as_ref().filter(|_| !probing).map(|availability| availability.status);
        let mark = status_icon(status);
        let label = text(harness.option.label.as_str()).size(panel_style::LIST_TEXT_SIZE);
        let word = text(status_word(status)).size(panel_style::TEXT_SIZE).style(move |theme: &Theme| match status {
            Some(AvailabilityStatus::Ready) => panel_style::success_text(theme),
            Some(AvailabilityStatus::Unknown) | None => panel_style::muted(theme),
            Some(_) => panel_style::danger_text(theme),
        });
        let action: Element<'a, Message> = if selected == Some(id) {
            text("New sessions").size(panel_style::TEXT_SIZE).style(panel_style::primary_text).into()
        } else if harness.option.available {
            button(text("Use").size(panel_style::TEXT_SIZE))
                .padding([2, 8])
                .style(panel_style::action_button)
                .on_press(Message::Pick(id.to_owned()))
                .into()
        } else {
            space::horizontal().width(0).into()
        };
        let title = row![mark, label, word, space::horizontal(), action].spacing(8).align_y(iced::Center);
        let mut body = column![title].spacing(4);
        let version = harness.availability.as_ref().and_then(|availability| availability.version.as_deref());
        if let Some(detail) = harness.option.detail.as_deref().filter(|detail| Some(*detail) != version) {
            body = body.push(text(detail).size(panel_style::TEXT_SIZE).style(panel_style::muted));
        }
        if let Some(version) = version {
            body = body.push(text(format!("Version {version}")).size(panel_style::TEXT_SIZE).style(panel_style::muted));
        }
        if id == loa_adapter_opencode::mapper::HARNESS {
            body = body.push(text(format!("Server {OPENCODE_URL}")).size(panel_style::TEXT_SIZE).style(panel_style::muted));
        } else {
            if let Some(executable) = &harness.executable {
                body = body.push(text(executable.display().to_string()).size(panel_style::TEXT_SIZE).font(Font::MONOSPACE).style(panel_style::muted));
            }
            let draft = self.drafts.get(id).map_or("", String::as_str);
            let field = text_input(default_program(id), draft)
                .size(panel_style::TEXT_SIZE)
                .padding([4, 8])
                .style(style::input)
                .on_input(move |value| Message::EditExecutable(id.to_owned(), value))
                .on_submit(Message::SaveExecutable(id.to_owned()));
            let configured = harness.configured.as_ref().map(|path| path.display().to_string());
            let changed = configured.as_deref().unwrap_or("") != draft.trim();
            let apply = button(text("Save").size(panel_style::TEXT_SIZE))
                .padding([4, 8])
                .style(panel_style::action_button)
                .on_press_maybe(changed.then(|| Message::SaveExecutable(id.to_owned())));
            body = body.push(row![field, apply].spacing(6).align_y(iced::Center));
        }
        container(body).padding(10).width(Length::Fill).style(style::card).into()
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

fn status_icon<'a>(status: Option<AvailabilityStatus>) -> Element<'a, Message> {
    let (icon, color): (Icon, fn(&Theme) -> iced::Color) = match status {
        Some(AvailabilityStatus::Ready) => (Icon::Check, |theme| theme.extended_palette().success.base.color),
        None | Some(AvailabilityStatus::Unknown) => (Icon::CircleDot, style::muted_color),
        Some(_) => (Icon::TriangleAlert, |theme| theme.extended_palette().danger.base.color),
    };
    loa_ui_kit::icons::icon(icon, 14.0).style(move |theme: &Theme, _| svg::Style { color: Some(color(theme)) }).into()
}

/// The executable field's placeholder: what runs when it is empty.
fn default_program(id: &str) -> &'static str {
    match id {
        loa_harness::claude::HARNESS => "claude (from PATH)",
        loa_harness::codex::HARNESS => "codex (from PATH)",
        loa_harness::acp::HARNESS => "ACP agent (opencode or gemini from PATH)",
        _ => "omp (from PATH)",
    }
}

impl Area for State {
    type Message = Message;

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

    fn palette_commands(&self, ctx: &Ctx) -> Vec<palette::Command> {
        let switch = ctx.services.harnesses.list.iter().filter(|harness| harness.option.available).map(|harness| {
            let id = AreaId::Harness.command(&harness.option.id);
            palette::Command::new(id, format!("Switch harness: {}", harness.option.label), "Harness")
        });
        let recheck = palette::Command::new(AreaId::Harness.command(RECHECK), "Recheck harness availability", "Harness");
        switch.chain([recheck]).collect()
    }

    fn run_command(&mut self, id: &str, ctx: &mut Ctx) -> Option<Task<Message>> {
        let name = id.strip_prefix(AreaId::Harness.key())?.strip_prefix('.')?;
        if name == RECHECK {
            ctx.open_tab(TAB);
            return Some(self.probe_all(ctx));
        }
        Some(self.switch(name.to_owned(), ctx))
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
