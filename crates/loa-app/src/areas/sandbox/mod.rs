//! `sandbox_task`: every session on a harness that takes host tools gets the tool from
//! [`SandboxService`], with the providers this machine can run.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{button, column, container, row, scrollable, space, toggler};
use iced::{Alignment, Element, Fill, Length, Subscription, Task};
use loa_contracts::harness::SessionConfig;
use loa_core::PatchSink;
use loa_session::library::Library;
use loa_sandbox::isolation::{apply_patch, backend_name};
use loa_sandbox::worker::WorkerJournal;
use loa_sandbox::{Availability, ProviderStatus, RunEntry, RunLog, RunState, SandboxConfig, SandboxError, SandboxEvent, SandboxKind, SandboxService};
use loa_ui_kit::icons::tinted;
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, pad_x, sp, text_size, width};
use loa_ui_kit::widgets::{Field, Form, Tab, badge, card, form_view, label, tab_strip, text_button};
use loa_ui_panel::diff;
use loa_view::panel::{DetailState, ToolDetail, ToolInfo};
use loa_view::theme::Icon;
use tokio::sync::broadcast;

use crate::area::{Area, AreaCommand, AreaMessage, Ctx, HostToolSet, PanelTab, Request, Target};
use loa_runtime::session::SessionEvent;
use crate::watch::{PtrKey, watch};

mod peer;
#[cfg(test)]
mod tests;

/// Id of the Sandbox panel tab.
pub const TAB: &str = "sandbox";
/// The settings file under the data directory.
const SETTINGS_FILE: &str = "sandbox.json";

#[derive(Clone, Debug)]
pub enum Message {
    /// A run started, finished or failed.
    Event(SandboxEvent),
    /// What the providers' probe found.
    Probed(Vec<ProviderStatus>),
    /// Probe the providers again.
    Refresh,
    /// Show the runs or the settings.
    View(View),
    Select(u64),
    Diff(diff::Message),
    /// Apply the run's patch to its workspace.
    Apply(u64),
    Applied { run: u64, result: Result<(), String> },
    /// Show the worker session of a run.
    OpenWorker(String),
    /// A text setting, by field key, was edited.
    Edit(&'static str, String),
    Network(bool),
    Save,
    /// A peer's sandbox run on this device failed.
    PeerFailed(peer::Served),
}

/// What the Sandbox tab shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Runs,
    Settings,
}

/// One text setting: its provider, its form field, and how it reads from and writes to the config.
struct Setting {
    kind: SandboxKind,
    field: Field,
    read: fn(&SandboxConfig) -> String,
    write: fn(&mut SandboxConfig, &str),
}

const fn setting(kind: SandboxKind, key: &'static str, label: &'static str, placeholder: &'static str, read: fn(&SandboxConfig) -> String, write: fn(&mut SandboxConfig, &str)) -> Setting {
    Setting { kind, field: Field { key, label, placeholder }, read, write }
}

/// The settings form, in provider order. The VM ones show with `sandbox-vm`, and are kept without it.
const SETTINGS: [Setting; 6] = [
    setting(SandboxKind::Docker, "docker.image", "Default image", "debian:stable-slim", |c| c.docker_image.clone(), |c, v| {
        c.docker_image = if v.trim().is_empty() { SandboxConfig::default().docker_image } else { v.trim().to_owned() }
    }),
    setting(SandboxKind::Firecracker, "firecracker.kernel", "Guest kernel", "/path/to/vmlinux", |c| shown(&c.firecracker_kernel), |c, v| c.firecracker_kernel = path(v)),
    setting(SandboxKind::Firecracker, "firecracker.rootfs", "Root filesystem", "/path/to/rootfs.ext4", |c| shown(&c.firecracker_rootfs), |c, v| c.firecracker_rootfs = path(v)),
    setting(SandboxKind::Vm, "vm.disk", "Disk image", "/path/to/disk.img", |c| shown(&c.vm_disk), |c, v| c.vm_disk = path(v)),
    setting(SandboxKind::Vm, "vm.kernel", "Kernel (optional)", "boot from the disk", |c| shown(&c.vm_kernel), |c, v| c.vm_kernel = path(v)),
    // Hosts separated by commas or spaces.
    setting(SandboxKind::Ssh, "ssh.hosts", "Hosts", "the hosts of ~/.ssh/config", |c| c.ssh_hosts.join(", "), |c, v| {
        c.ssh_hosts = v.split([',', ' ']).filter(|host| !host.is_empty()).map(str::to_owned).collect()
    }),
];

/// The providers in the order the settings list them.
const PROVIDERS: [SandboxKind; 6] = [SandboxKind::Local, SandboxKind::Docker, SandboxKind::Firecracker, SandboxKind::Vm, SandboxKind::Ssh, SandboxKind::Peer];

fn shown(path: &Option<PathBuf>) -> String {
    path.as_ref().map(|path| path.display().to_string()).unwrap_or_default()
}

fn path(text: &str) -> Option<PathBuf> {
    Some(text.trim()).filter(|text| !text.is_empty()).map(PathBuf::from)
}

/// A provider's text settings as typed.
struct Section {
    kind: SandboxKind,
    fields: Vec<Field>,
    values: BTreeMap<&'static str, String>,
}

impl Form for Section {
    fn fields(&self) -> &[Field] {
        &self.fields
    }

    fn get(&self, key: &str) -> String {
        self.values.get(key).cloned().unwrap_or_default()
    }

    fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        if let Some(text) = self.values.get_mut(key) {
            *text = value.to_owned();
        }
        Ok(())
    }
}

/// The settings form over `config`: a section per provider, and the network switch.
struct Draft {
    sections: Vec<Section>,
    network: bool,
}

impl Draft {
    fn of(config: &SandboxConfig) -> Self {
        let section = |kind| {
            let settings = || SETTINGS.iter().filter(move |setting| setting.kind == kind);
            Section { kind, fields: settings().map(|setting| setting.field).collect(), values: settings().map(|setting| (setting.field.key, (setting.read)(config))).collect() }
        };
        Self { sections: PROVIDERS.into_iter().map(section).collect(), network: config.docker_network }
    }

    fn config(&self) -> SandboxConfig {
        let mut config = SandboxConfig { docker_network: self.network, ..SandboxConfig::default() };
        for setting in &SETTINGS {
            let section = self.sections.iter().find(|section| section.kind == setting.kind);
            (setting.write)(&mut config, &section.map(|section| section.get(setting.field.key)).unwrap_or_default());
        }
        config
    }

    fn edit(&mut self, key: &str, value: &str) {
        for section in &mut self.sections {
            let _ = section.set(key, value);
        }
    }
}

/// Keeps worker sessions in the app's session library, so they are journaled, listed and opened like any other.
struct LibraryJournal(Arc<Library>);

impl WorkerJournal for LibraryJournal {
    fn open(&self, session_id: &str) -> loa_sandbox::Result<Arc<dyn PatchSink>> {
        let live = self.0.create(session_id).map_err(|error| SandboxError::Io(std::io::Error::other(format!("can't create the worker session {session_id}: {error}"))))?;
        Ok(live.writer)
    }
}

pub struct State {
    service: SandboxService,
    /// Kept to subscribe the run feed from.
    events: Arc<broadcast::Receiver<SandboxEvent>>,
    /// Kept to subscribe the failures of peers' runs on this device from.
    served: Arc<broadcast::Receiver<peer::Served>>,
    runs: RunLog,
    /// The last probe; `None` until it answers.
    statuses: Option<Vec<ProviderStatus>>,
    probing: bool,
    view: View,
    selected: Option<u64>,
    /// The selected run's patch.
    diff: diff::State,
    applying: Option<u64>,
    draft: Draft,
    settings: PathBuf,
}

impl State {
    /// The runs, oldest first.
    pub fn runs(&self) -> &RunLog {
        &self.runs
    }

    /// Every `sandbox_task` run, as [`Message::Event`] expects them.
    pub fn subscribe(&self) -> broadcast::Receiver<SandboxEvent> {
        self.service.subscribe()
    }

    fn probe(&mut self) -> Task<Message> {
        self.probing = true;
        Task::perform(self.service.probe(), Message::Probed)
    }

    /// Lists `sandbox_task` among the app tools with one row per provider.
    fn publish(&self, ctx: &mut Ctx) {
        let description = format!(
            "Runs a task as a worker agent inside a sandbox; its model calls stay on this machine. With isolation it works on a {} checkout and returns a patch.",
            backend_name()
        );
        let details = self.statuses.iter().flatten().map(|status| ToolDetail {
            label: kind_label(status.kind).to_owned(),
            state: match status.availability {
                Availability::Ready => DetailState::Ready,
                Availability::NotConfigured => DetailState::NotConfigured,
                Availability::Unavailable => DetailState::Unavailable,
            },
            text: status.detail.clone(),
        });
        ctx.set_app_tool(ToolInfo::new(loa_sandbox::tool::NAME, description).with_details(details.collect()));
    }

    fn event(&mut self, event: SandboxEvent, ctx: &mut Ctx) -> Task<Message> {
        let ended = match &event {
            SandboxEvent::Started(_) => None,
            SandboxEvent::Finished { run, .. } | SandboxEvent::Failed { run, .. } => Some(*run),
        };
        self.runs.record(event);
        let Some(entry) = ended.and_then(|run| self.runs.get(run)) else { return Task::none() };
        let (run, worker) = (entry.run.run, entry.run.worker.clone());
        // The worker is done: release its journal unless the user opened it.
        if ctx.runtime.sessions().get(&worker).is_none()
            && let Err(error) = ctx.runtime.library().close(&worker)
        {
            ctx.notify(format!("Can't close the worker session {worker}: {error}"));
        }
        if entry.pending_patch().is_none() || !ctx.runtime.sessions().is_active(&entry.run.parent) {
            return Task::none();
        }
        self.view = View::Runs;
        ctx.open_tab(TAB);
        self.select(run, ctx)
    }

    fn select(&mut self, run: u64, ctx: &mut Ctx) -> Task<Message> {
        self.selected = Some(run);
        let patch = match self.runs.get(run).map(|entry| &entry.state) {
            Some(RunState::Finished { patch: Some(patch), .. }) => patch.as_str(),
            _ => "",
        };
        let files = diff::parse_unified(patch).unwrap_or_else(|error| {
            ctx.notify(format!("Can't read the sandbox patch: {error}"));
            Vec::new()
        });
        self.diff.set_files(files).map(Message::Diff)
    }

    fn apply(&mut self, run: u64, ctx: &mut Ctx) -> Task<Message> {
        let Some(entry) = self.runs.get(run) else { return Task::none() };
        let Some(patch) = entry.pending_patch() else {
            ctx.notify("This run has no patch left to apply");
            return Task::none();
        };
        let (workspace, patch) = (entry.run.workspace.clone(), patch.as_bytes().to_vec());
        self.applying = Some(run);
        Task::perform(
            async move { tokio::task::spawn_blocking(move || apply_patch(&workspace, &patch).map_err(|error| error.to_string())).await.unwrap_or_else(|error| Err(error.to_string())) },
            move |result| Message::Applied { run, result },
        )
    }

    /// Saves the settings, hands them to the service for new sessions and probes again.
    fn save(&mut self, ctx: &mut Ctx) -> Task<Message> {
        let config = self.draft.config();
        if let Err(error) = config.save(&self.settings) {
            ctx.notify(format!("Can't save the sandbox settings {}: {error}", self.settings.display()));
            return Task::none();
        }
        self.draft = Draft::of(&config);
        self.service.set_config(config);
        ctx.notify("Sandbox settings saved; new sessions use them");
        self.probe()
    }

    fn view<'a>(&'a self, ctx: &'a Ctx) -> Element<'a, Message> {
        let tab = |label, view| Tab { label, icon: None, active: self.view == view, on: Message::View(view), id: None, trailing: None };
        let running = self.runs.running();
        let running = (running > 0).then(|| badge(format!("{running} running"), Tone::Normal));
        let header = row![tab_strip([tab("Runs", View::Runs), tab("Settings", View::Settings)])].push(running).spacing(sp(SpaceRole::Xs)).align_y(Alignment::Center);
        let body = match self.view {
            View::Runs => self.runs_view(ctx),
            View::Settings => self.settings_view(),
        };
        column![container(header).padding([sp(SpaceRole::S), sp(SpaceRole::M)]), body].height(Fill).into()
    }

    fn runs_view<'a>(&'a self, ctx: &'a Ctx) -> Element<'a, Message> {
        if self.runs.entries().is_empty() {
            return container(label(self.empty_text(), TextRole::Label, Tone::Muted)).padding([sp(SpaceRole::Xs), sp(SpaceRole::M)]).width(Fill).into();
        }
        let rows = self.runs.entries().iter().rev().map(|entry| self.run_row(entry, ctx));
        let list = container(scrollable(column(rows).padding(pad_x(SpaceRole::S)))).max_height(width(LayoutRole::ListWidth) * 0.6);
        let selected = self.selected.and_then(|run| self.runs.get(run));
        let diff = selected.filter(|_| !self.diff.files().is_empty()).map(|_| self.diff.view().map(Message::Diff));
        column![list].push(selected.map(|entry| self.detail(entry))).push(diff).spacing(sp(SpaceRole::S)).height(Fill).into()
    }

    /// What the empty run list says: where runs come from and which providers can take them.
    fn empty_text(&self) -> String {
        let Some(statuses) = &self.statuses else { return "Checking which sandboxes this machine can run…".into() };
        let ready: Vec<_> = statuses.iter().filter(|status| status.availability == Availability::Ready).map(|status| kind_label(status.kind)).collect();
        if ready.is_empty() {
            "No sandbox provider can run on this machine, so sessions don't get sandbox_task. The Tools tab and the settings say what each one needs.".into()
        } else {
            format!("No sandbox_task runs yet. Sessions on harnesses with host tools can call it; ready here: {}.", ready.join(", "))
        }
    }

    fn run_row<'a>(&'a self, entry: &'a RunEntry, ctx: &'a Ctx) -> Element<'a, Message> {
        let run = &entry.run;
        let (icon, tone, summary) = match &entry.state {
            RunState::Running => (Icon::CircleDot, Tone::Accent, "Running…".to_owned()),
            RunState::Failed(_) => (Icon::TriangleAlert, Tone::Danger, "Failed".to_owned()),
            RunState::Finished { files, applied, patch, .. } => {
                let summary = match (patch, files.len()) {
                    (None, _) => "Done in the workspace".to_owned(),
                    (Some(_), 0) => "Done, no changes".to_owned(),
                    (Some(_), count) => {
                        let (added, removed): (usize, usize) = (files.iter().map(|file| file.added).sum(), files.iter().map(|file| file.removed).sum());
                        let state = if *applied { "applied" } else { "patch" };
                        format!("+{added} −{removed} · {count} {} · {state}", if count == 1 { "file" } else { "files" })
                    }
                };
                (Icon::Check, Tone::Ok, summary)
            }
        };
        let session = ctx.runtime.sessions().get(&run.parent).and_then(|session| session.summary()).map_or_else(|| run.parent.clone(), |summary| summary.title);
        let title = row![tinted(icon, TextRole::Body, tone), badge(kind_label(run.kind), Tone::Normal), label(loa_util::text::first_line(&run.task, 72), TextRole::Label, Tone::Normal)];
        let meta = label(format!("{summary} · {session}"), TextRole::Caption, Tone::Muted);
        let content = column![title.spacing(sp(SpaceRole::S)).align_y(Alignment::Center), meta];
        button(content).width(Fill).padding([sp(SpaceRole::Xs), sp(SpaceRole::S)]).style(style::button(ButtonKind::Row { selected: self.selected == Some(run.run) })).on_press(Message::Select(run.run)).into()
    }

    fn detail<'a>(&'a self, entry: &'a RunEntry) -> Element<'a, Message> {
        let run = &entry.run;
        let isolation = if run.isolated { format!("isolated {} checkout of", backend_name()) } else { "directly in".into() };
        let place = label(format!("{} sandbox, {isolation} {}", kind_label(run.kind), run.workspace.display()), TextRole::Mono, Tone::Muted);
        let outcome = match &entry.state {
            RunState::Running => None,
            RunState::Failed(error) => Some(label(error.as_str(), TextRole::Label, Tone::Danger)),
            RunState::Finished { final_message, .. } => final_message.as_deref().filter(|message| !message.trim().is_empty()).map(|message| label(message, TextRole::Label, Tone::Muted)),
        };
        let applying = self.applying == Some(run.run);
        let patch = match &entry.state {
            RunState::Finished { applied: true, .. } => Some(label("Patch applied", TextRole::Label, Tone::Ok).into()),
            _ if entry.pending_patch().is_some() => Some(text_button(if applying { "Applying…" } else { "Apply patch" }, ButtonKind::Secondary, (!applying).then_some(Message::Apply(run.run)))),
            _ => None,
        };
        let actions = row![].push(patch).push(text_button("Open worker session", ButtonKind::Secondary, Some(Message::OpenWorker(run.worker.clone()))));
        let body = column![label(run.task.as_str(), TextRole::Label, Tone::Normal), place].push(outcome).push(actions.spacing(sp(SpaceRole::Xs)).align_y(Alignment::Center));
        card(body.spacing(sp(SpaceRole::Xs))).width(Fill).into()
    }

    fn settings_view(&self) -> Element<'_, Message> {
        let errors = BTreeMap::new();
        let sections = self.draft.sections.iter().filter(|section| cfg!(feature = "sandbox-vm") || !matches!(section.kind, SandboxKind::Firecracker | SandboxKind::Vm)).map(|section| {
            let fields = (!section.fields.is_empty()).then(|| form_view(section, &errors, Message::Edit, Some(Message::Save)));
            let network = (section.kind == SandboxKind::Docker).then(|| {
                toggler(self.draft.network).label("Containers may use the network").text_size(text_size(TextRole::Label)).on_toggle(Message::Network)
            });
            self.provider(section.kind, column![].push(fields).push(network).spacing(sp(SpaceRole::S)))
        });
        let changed = self.draft.config() != *self.service.config();
        let recheck = text_button(if self.probing { "Checking…" } else { "Recheck" }, ButtonKind::Secondary, (!self.probing).then_some(Message::Refresh));
        let actions = row![space::horizontal(), recheck, text_button("Save", ButtonKind::Secondary, changed.then_some(Message::Save))].spacing(sp(SpaceRole::Xs));
        scrollable(column(sections).push(actions).spacing(sp(SpaceRole::S)).padding(pad_x(SpaceRole::M))).height(Length::Fill).into()
    }

    /// One provider's section: its availability and its settings.
    fn provider<'a>(&'a self, kind: SandboxKind, settings: iced::widget::Column<'a, Message>) -> Element<'a, Message> {
        let status = self.statuses.iter().flatten().find(|status| status.kind == kind);
        let (icon, tone, word) = match status.map(|status| status.availability) {
            None => (Icon::CircleDot, Tone::Muted, "Checking…"),
            Some(Availability::Ready) => (Icon::Check, Tone::Ok, "Ready"),
            Some(Availability::NotConfigured) => (Icon::CircleDot, Tone::Muted, "Not configured"),
            Some(Availability::Unavailable) => (Icon::TriangleAlert, Tone::Danger, "Unavailable"),
        };
        let title = row![tinted(icon, TextRole::Body, tone), label(kind_label(kind), TextRole::Body, Tone::Normal), label(word, TextRole::Label, tone)].spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
        let detail = status.map(|status| label(status.detail.as_str(), TextRole::Caption, Tone::Muted));
        container(column![title].push(detail).push(settings).spacing(sp(SpaceRole::Xs))).padding(sp(SpaceRole::M)).width(Fill).style(style::container(Surface::Card)).into()
    }
}

/// How the UI names a provider.
fn kind_label(kind: SandboxKind) -> &'static str {
    match kind {
        SandboxKind::Local => "Local",
        SandboxKind::Docker => "Docker",
        SandboxKind::Firecracker => "Firecracker",
        SandboxKind::Vm => "VM (qemu)",
        SandboxKind::Ssh => "SSH host",
        SandboxKind::Peer => "Peer",
    }
}

/// Shows the settings in the Sandbox tab.
fn show_settings(state: &mut State, ctx: &mut Ctx) {
    state.view = View::Settings;
    ctx.open_tab(TAB);
}

impl AreaMessage for Message {
    type Area = State;
}

impl Area for State {
    const ID: &'static str = "sandbox";
    const COMMANDS: &'static [AreaCommand<Self>] = &[
        AreaCommand::<Self>::new("sandbox.refresh", "Recheck sandbox availability", "Sandbox", |state, _, ctx| {
            show_settings(state, ctx);
            state.probe()
        }),
        AreaCommand::<Self>::new("sandbox.settings", "Sandbox settings", "Sandbox", |state, _, ctx| {
            show_settings(state, ctx);
            Task::none()
        }),
    ];

    type Message = Message;

    fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let settings = ctx.services.storage.dirs.data.join(SETTINGS_FILE);
        let config = SandboxConfig::load(&settings).unwrap_or_else(|error| {
            ctx.notify(format!("Can't read the sandbox settings {}: {error}; using the defaults", settings.display()));
            SandboxConfig::default()
        });
        let journal = Arc::new(LibraryJournal(Arc::clone(ctx.runtime.library())));
        let service = SandboxService::new(config, journal).with_peers(Arc::new(peer::NodeLink(ctx.node.clone())));
        let (failures, served) = broadcast::channel(16);
        peer::register(&ctx.node, Arc::new(peer::ServeStub { stub: service.stub().map(PathBuf::from), failures }));
        let mut state = Self {
            events: Arc::new(service.subscribe()),
            served: Arc::new(served),
            draft: Draft::of(service.config()),
            service,
            runs: RunLog::default(),
            statuses: None,
            probing: false,
            view: View::Runs,
            selected: None,
            diff: diff::State::new(),
            applying: None,
            settings,
        };
        state.publish(ctx);
        let probe = state.probe();
        (state, probe)
    }

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Event(event) => return self.event(event, ctx),
            Message::Probed(statuses) => {
                self.probing = false;
                self.statuses = Some(statuses);
                self.publish(ctx);
            }
            Message::Refresh => return self.probe(),
            Message::View(view) => self.view = view,
            Message::Select(run) => return self.select(run, ctx),
            Message::Diff(message) => return self.diff.update(message).0.map(Message::Diff),
            Message::Apply(run) => return self.apply(run, ctx),
            Message::Applied { run, result } => {
                self.applying = None;
                if ctx.report("Can't apply the sandbox patch", result).is_some() {
                    self.runs.mark_applied(run);
                    if let Some(entry) = self.runs.get(run) {
                        ctx.notify(format!("Applied the sandbox patch to {}", entry.run.workspace.display()));
                    }
                }
            }
            Message::OpenWorker(worker) => ctx.request(Request::OpenSession(worker)),
            Message::Edit(key, value) => self.draft.edit(key, &value),
            Message::Network(network) => self.draft.network = network,
            Message::Save => return self.save(ctx),
            Message::PeerFailed(served) => ctx.notify(format!("A sandbox run for peer {} failed on this device: {}", served.peer, served.error)),
        }
        Task::none()
    }

    fn panel_tabs(&self) -> Vec<PanelTab> {
        vec![PanelTab::new(TAB, "Sandbox", false)]
    }

    fn view_tab<'a>(&'a self, id: &str, ctx: &'a Ctx) -> Option<Element<'a, Message>> {
        (id == TAB).then(|| self.view(ctx))
    }

    fn host_tools(&self, config: &SessionConfig, target: &Target<'_>, ctx: &Ctx) -> Option<HostToolSet> {
        // The worker runs on the session's harness, through its host tools.
        if !target.capabilities.host_tools {
            return None;
        }
        let adapter = ctx.runtime.harnesses().registry.get(target.harness)?;
        let (spec, handler) = self.service.host_tool(adapter, config)?;
        Some(HostToolSet { specs: vec![spec], handler })
    }

    fn on_session_event(&mut self, event: &SessionEvent, _: &mut Ctx) -> Task<Message> {
        // The patch shown belongs to the session the user just left.
        if let SessionEvent::Activated(Some(id)) = event
            && self.selected.and_then(|run| self.runs.get(run)).is_none_or(|entry| entry.run.parent != id.as_str())
        {
            self.selected = None;
            return self.diff.set_files(Vec::new()).map(Message::Diff);
        }
        Task::none()
    }

    /// Every run event and peer failure from now on; skips what a feed fell behind on.
    fn subscription(&self) -> Subscription<Message> {
        let (events, served) = (Arc::clone(&self.events), Arc::clone(&self.served));
        Subscription::batch([
            watch(PtrKey(&self.events), move || events.resubscribe(), |event| event.ok().map(Message::Event)),
            watch(PtrKey(&self.served), move || served.resubscribe(), |failure| failure.ok().map(Message::PeerFailed)),
        ])
    }
}
