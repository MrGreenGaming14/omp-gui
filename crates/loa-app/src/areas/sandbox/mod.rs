//! `sandbox_task`: every session on a harness that takes host tools gets the
//! tool from [`SandboxService`], with the providers this machine can run.
//! The Tools panel lists each provider's availability and why not; the
//! Sandbox tab lists the runs, shows the patch an isolated run returns,
//! applies it to the workspace and holds the provider settings
//! (`<data>/sandbox.json`). Worker sessions are journaled in the session
//! library like any other session and open from their run.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use iced::futures::{SinkExt, Stream};
use iced::widget::{button, column, container, row, scrollable, space, svg, text, text_input, toggler};
use iced::{Element, Font, Length, Subscription, Task, Theme};
use loa_contracts::harness::SessionConfig;
use loa_core::PatchSink;
use loa_features::library::Library;
use loa_sandbox::isolation::{apply_patch, backend_name};
use loa_sandbox::worker::WorkerJournal;
use loa_sandbox::{
    Availability, ProviderStatus, RunEntry, RunLog, RunState, SandboxConfig, SandboxError, SandboxEvent, SandboxKind,
    SandboxService,
};
use loa_ui_kit::icons::Icon;
use loa_ui_shell::palette;
use loa_ui_panel::diff;
use loa_ui_shell::theme::style;
use loa_ui_panel::tools::{DetailState, ToolDetail, ToolInfo};
use tokio::sync::broadcast;

use crate::area::{Area, AreaId, Ctx, HostToolSet, PanelTab, Request, SessionEvent, Target};

#[cfg(test)]
mod tests;
mod peer;

/// Id of the Sandbox panel tab.
pub const TAB: &str = "sandbox";
/// Palette command name that probes the providers again.
const REFRESH: &str = "refresh";
/// Palette command name that shows the settings.
const SETTINGS: &str = "settings";
/// The settings file under the data directory.
const SETTINGS_FILE: &str = "sandbox.json";

/// The tab's text sizes and colors, from the theme like the other panel tabs.
mod panel_style {
    use iced::Theme;
    use iced::widget::{button, text};
    use loa_ui_shell::theme::style;

    /// Size of row details.
    pub const TEXT_SIZE: f32 = 13.0;
    /// Size of provider names, matching the sidebar.
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

    /// A small outlined action inside a row.
    pub fn action_button(theme: &Theme, status: button::Status) -> button::Style {
        button::Style {
            border: iced::Border { color: style::border_color(theme), width: 1.0, radius: style::RADIUS.into() },
            ..style::row(theme, status)
        }
    }
}

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
    Edit(Field, String),
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

/// A text setting of the settings form. The VM ones show with `sandbox-vm`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(feature = "sandbox-vm"), allow(dead_code))]
pub enum Field {
    DockerImage,
    FirecrackerKernel,
    FirecrackerRootfs,
    VmDisk,
    VmKernel,
    SshHosts,
}

/// The settings form, as typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Draft {
    docker_image: String,
    docker_network: bool,
    firecracker_kernel: String,
    firecracker_rootfs: String,
    vm_disk: String,
    vm_kernel: String,
    /// Hosts separated by commas or spaces.
    ssh_hosts: String,
}

impl Draft {
    fn of(config: &SandboxConfig) -> Self {
        let path = |path: &Option<PathBuf>| path.as_ref().map(|path| path.display().to_string()).unwrap_or_default();
        Self {
            docker_image: config.docker_image.clone(),
            docker_network: config.docker_network,
            firecracker_kernel: path(&config.firecracker_kernel),
            firecracker_rootfs: path(&config.firecracker_rootfs),
            vm_disk: path(&config.vm_disk),
            vm_kernel: path(&config.vm_kernel),
            ssh_hosts: config.ssh_hosts.join(", "),
        }
    }

    fn config(&self) -> SandboxConfig {
        let path = |text: &str| Some(text.trim()).filter(|text| !text.is_empty()).map(PathBuf::from);
        let image = self.docker_image.trim();
        SandboxConfig {
            docker_image: if image.is_empty() { SandboxConfig::default().docker_image } else { image.to_owned() },
            docker_network: self.docker_network,
            firecracker_kernel: path(&self.firecracker_kernel),
            firecracker_rootfs: path(&self.firecracker_rootfs),
            vm_disk: path(&self.vm_disk),
            vm_kernel: path(&self.vm_kernel),
            ssh_hosts: self.ssh_hosts.split([',', ' ']).filter(|host| !host.is_empty()).map(str::to_owned).collect(),
        }
    }

    fn field(&mut self, field: Field) -> &mut String {
        match field {
            Field::DockerImage => &mut self.docker_image,
            Field::FirecrackerKernel => &mut self.firecracker_kernel,
            Field::FirecrackerRootfs => &mut self.firecracker_rootfs,
            Field::VmDisk => &mut self.vm_disk,
            Field::VmKernel => &mut self.vm_kernel,
            Field::SshHosts => &mut self.ssh_hosts,
        }
    }
}

/// Keeps worker sessions in the app's session library, so they are
/// journaled, listed and opened like any other session.
struct LibraryJournal(Arc<Library>);

impl WorkerJournal for LibraryJournal {
    fn open(&self, session_id: &str) -> loa_sandbox::Result<Arc<dyn PatchSink>> {
        let live = self.0.create(session_id).map_err(|error| {
            SandboxError::Io(std::io::Error::other(format!("can't create the worker session {session_id}: {error}")))
        })?;
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
    pub fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let settings = ctx.services.dirs.data.join(SETTINGS_FILE);
        let config = SandboxConfig::load(&settings).unwrap_or_else(|error| {
            ctx.notify(format!("Can't read the sandbox settings {}: {error}; using the defaults", settings.display()));
            SandboxConfig::default()
        });
        let journal = Arc::new(LibraryJournal(Arc::clone(&ctx.services.library)));
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
            "Runs a task as a worker agent inside a sandbox; its model calls stay on this machine. With isolation \
             it works on a {} checkout and returns a patch.",
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
        if ctx.sessions.get(&worker).is_none()
            && let Err(error) = ctx.services.library.close(&worker)
        {
            ctx.notify(format!("Can't close the worker session {worker}: {error}"));
        }
        let review = entry.pending_patch().is_some() && ctx.sessions.is_active(&entry.run.parent);
        if !review {
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
        match diff::parse_unified(patch) {
            Ok(files) => self.diff.set_files(files).map(Message::Diff),
            Err(error) => {
                ctx.notify(format!("Can't read the sandbox patch: {error}"));
                self.diff.set_files(Vec::new()).map(Message::Diff)
            }
        }
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
            async move {
                tokio::task::spawn_blocking(move || apply_patch(&workspace, &patch).map_err(|error| error.to_string()))
                    .await
                    .unwrap_or_else(|error| Err(error.to_string()))
            },
            move |result| Message::Applied { run, result },
        )
    }

    /// Saves the settings, hands them to the service (for sessions started
    /// from now on) and probes again.
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

    fn status(&self, kind: SandboxKind) -> Option<&ProviderStatus> {
        self.statuses.iter().flatten().find(|status| status.kind == kind)
    }

    fn view<'a>(&'a self, ctx: &'a Ctx) -> Element<'a, Message> {
        let tab = |label: &'a str, view: View| {
            button(text(label).size(panel_style::TEXT_SIZE))
                .padding([2, 6])
                .style(style::tab(self.view == view))
                .on_press(Message::View(view))
        };
        let running = self.runs.running();
        let mut header = row![tab("Runs", View::Runs), tab("Settings", View::Settings)].spacing(4).align_y(iced::Center);
        if running > 0 {
            header = header.push(container(text(format!("{running} running")).size(12)).padding([1, 6]).style(style::badge));
        }
        let header = header.push(space::horizontal());
        let body = match self.view {
            View::Runs => self.runs_view(ctx),
            View::Settings => self.settings_view(),
        };
        column![container(header).padding([8, 12]), body].height(Length::Fill).into()
    }

    fn runs_view<'a>(&'a self, ctx: &'a Ctx) -> Element<'a, Message> {
        if self.runs.entries().is_empty() {
            return container(text(self.empty_text()).size(panel_style::TEXT_SIZE).style(panel_style::muted))
                .padding([4, 12])
                .width(Length::Fill)
                .into();
        }
        let rows = self.runs.entries().iter().rev().map(|entry| self.run_row(entry, ctx));
        let list = container(scrollable(column(rows).spacing(2).padding([0, 8]))).max_height(220.0);
        let mut body = column![list].spacing(8);
        if let Some(entry) = self.selected.and_then(|run| self.runs.get(run)) {
            body = body.push(self.detail(entry));
            if !self.diff.files().is_empty() {
                body = body.push(self.diff.view().map(Message::Diff));
            }
        }
        body.height(Length::Fill).into()
    }

    /// What the empty run list says: where runs come from and which
    /// providers can take them.
    fn empty_text(&self) -> String {
        let Some(statuses) = &self.statuses else { return "Checking which sandboxes this machine can run…".into() };
        let ready: Vec<_> =
            statuses.iter().filter(|status| status.availability == Availability::Ready).map(|status| kind_label(status.kind)).collect();
        if ready.is_empty() {
            "No sandbox provider can run on this machine, so sessions don't get sandbox_task. The Tools tab and the \
             settings say what each one needs."
                .into()
        } else {
            format!(
                "No sandbox_task runs yet. Sessions on harnesses with host tools can call it; ready here: {}.",
                ready.join(", ")
            )
        }
    }

    fn run_row<'a>(&'a self, entry: &'a RunEntry, ctx: &'a Ctx) -> Element<'a, Message> {
        let run = &entry.run;
        let (icon, color, summary): (Icon, fn(&Theme) -> iced::Color, String) = match &entry.state {
            RunState::Running => (Icon::CircleDot, |theme| theme.extended_palette().primary.base.color, "Running…".into()),
            RunState::Failed(_) => (Icon::TriangleAlert, |theme| theme.extended_palette().danger.base.color, "Failed".into()),
            RunState::Finished { files, applied, patch, .. } => {
                let summary = match (patch, files.len()) {
                    (None, _) => "Done in the workspace".to_owned(),
                    (Some(_), 0) => "Done, no changes".to_owned(),
                    (Some(_), count) => {
                        let added: usize = files.iter().map(|file| file.added).sum();
                        let removed: usize = files.iter().map(|file| file.removed).sum();
                        let state = if *applied { "applied" } else { "patch" };
                        format!("+{added} −{removed} · {count} {} · {state}", if count == 1 { "file" } else { "files" })
                    }
                };
                (Icon::Check, |theme| theme.extended_palette().success.base.color, summary)
            }
        };
        let mark = loa_ui_kit::icons::icon(icon, 14.0).style(move |theme: &Theme, _| svg::Style { color: Some(color(theme)) });
        let kind = container(text(kind_label(run.kind)).size(12)).padding([1, 6]).style(style::badge);
        let session = ctx
            .sessions
            .get(&run.parent)
            .and_then(|session| session.summary())
            .map_or_else(|| run.parent.clone(), |summary| summary.title);
        let title = row![mark, kind, text(loa_util::text::first_line(&run.task, 72)).size(panel_style::TEXT_SIZE)].spacing(8).align_y(iced::Center);
        let meta = text(format!("{summary} · {session}")).size(12).style(panel_style::muted);
        button(column![title, meta].spacing(2))
            .width(Length::Fill)
            .padding([6, 8])
            .style(style::row_for(self.selected == Some(run.run)))
            .on_press(Message::Select(run.run))
            .into()
    }

    fn detail<'a>(&'a self, entry: &'a RunEntry) -> Element<'a, Message> {
        let run = &entry.run;
        let mut body = column![text(run.task.as_str()).size(panel_style::TEXT_SIZE)].spacing(6);
        let isolation = if run.isolated { format!("isolated {} checkout of", backend_name()) } else { "directly in".into() };
        body = body.push(
            text(format!("{} sandbox, {isolation} {}", kind_label(run.kind), run.workspace.display()))
                .size(12)
                .font(Font::MONOSPACE)
                .style(panel_style::muted),
        );
        match &entry.state {
            RunState::Running => {}
            RunState::Failed(error) => body = body.push(text(error.as_str()).size(panel_style::TEXT_SIZE).style(panel_style::danger_text)),
            RunState::Finished { final_message, .. } => {
                if let Some(message) = final_message.as_deref().filter(|message| !message.trim().is_empty()) {
                    body = body.push(text(message).size(panel_style::TEXT_SIZE).style(panel_style::muted));
                }
            }
        }
        let mut actions = row![].spacing(6).align_y(iced::Center);
        match &entry.state {
            RunState::Finished { applied: true, .. } => {
                actions = actions.push(text("Patch applied").size(panel_style::TEXT_SIZE).style(panel_style::success_text));
            }
            _ if entry.pending_patch().is_some() => {
                let applying = self.applying == Some(run.run);
                actions = actions.push(
                    button(text(if applying { "Applying…" } else { "Apply patch" }).size(panel_style::TEXT_SIZE))
                        .padding([4, 10])
                        .style(panel_style::action_button)
                        .on_press_maybe((!applying).then_some(Message::Apply(run.run))),
                );
            }
            _ => {}
        }
        actions = actions.push(
            button(text("Open worker session").size(panel_style::TEXT_SIZE))
                .padding([4, 10])
                .style(panel_style::action_button)
                .on_press(Message::OpenWorker(run.worker.clone())),
        );
        body = body.push(actions);
        container(body).padding(10).width(Length::Fill).style(style::card).into()
    }

    fn settings_view(&self) -> Element<'_, Message> {
        let field = |label: &'static str, placeholder: &'static str, field: Field, value: &str| {
            let input = text_input(placeholder, value)
                .size(panel_style::TEXT_SIZE)
                .padding([4, 8])
                .style(style::input)
                .on_input(move |value| Message::Edit(field, value))
                .on_submit(Message::Save);
            column![text(label).size(12).style(panel_style::muted), input].spacing(3)
        };
        let draft = &self.draft;
        let docker = column![
            field("Default image", "debian:stable-slim", Field::DockerImage, &draft.docker_image),
            toggler(draft.docker_network)
                .label("Containers may use the network")
                .text_size(panel_style::TEXT_SIZE)
                .size(16)
                .on_toggle(Message::Network),
        ]
        .spacing(8);
        let ssh = field("Hosts", "the hosts of ~/.ssh/config", Field::SshHosts, &draft.ssh_hosts);
        let sections = column![self.provider(SandboxKind::Local, None), self.provider(SandboxKind::Docker, Some(docker.into()))];
        #[cfg(feature = "sandbox-vm")]
        let sections = sections
            .push(self.provider(SandboxKind::Firecracker, Some(column![
                field("Guest kernel", "/path/to/vmlinux", Field::FirecrackerKernel, &draft.firecracker_kernel),
                field("Root filesystem", "/path/to/rootfs.ext4", Field::FirecrackerRootfs, &draft.firecracker_rootfs),
            ].spacing(8).into())))
            .push(self.provider(SandboxKind::Vm, Some(column![
                field("Disk image", "/path/to/disk.img", Field::VmDisk, &draft.vm_disk),
                field("Kernel (optional)", "boot from the disk", Field::VmKernel, &draft.vm_kernel),
            ].spacing(8).into())));
        let sections = sections.push(self.provider(SandboxKind::Ssh, Some(ssh.into()))).push(self.provider(SandboxKind::Peer, None)).spacing(8);
        let changed = draft.config() != *self.service.config();
        let save = button(text("Save").size(panel_style::TEXT_SIZE))
            .padding([4, 10])
            .style(panel_style::action_button)
            .on_press_maybe(changed.then_some(Message::Save));
        let recheck = button(text(if self.probing { "Checking…" } else { "Recheck" }).size(panel_style::TEXT_SIZE))
            .padding([4, 10])
            .style(panel_style::action_button)
            .on_press_maybe((!self.probing).then_some(Message::Refresh));
        let actions = row![space::horizontal(), recheck, save].spacing(6);
        scrollable(column![sections, actions].spacing(10).padding([0, 12])).height(Length::Fill).into()
    }

    /// One provider's section: its availability and its settings.
    fn provider<'a>(&'a self, kind: SandboxKind, settings: Option<Element<'a, Message>>) -> Element<'a, Message> {
        let status = self.status(kind);
        let availability = status.map(|status| status.availability);
        let (icon, color, word): (Icon, fn(&Theme) -> iced::Color, &str) = match availability {
            None => (Icon::CircleDot, style::muted_color, "Checking…"),
            Some(Availability::Ready) => (Icon::Check, |theme| theme.extended_palette().success.base.color, "Ready"),
            Some(Availability::NotConfigured) => (Icon::CircleDot, style::muted_color, "Not configured"),
            Some(Availability::Unavailable) => (Icon::TriangleAlert, |theme| theme.extended_palette().danger.base.color, "Unavailable"),
        };
        let mark = loa_ui_kit::icons::icon(icon, 14.0).style(move |theme: &Theme, _| svg::Style { color: Some(color(theme)) });
        let word = text(word).size(panel_style::TEXT_SIZE).style(move |theme: &Theme| match availability {
            Some(Availability::Ready) => panel_style::success_text(theme),
            Some(Availability::Unavailable) => panel_style::danger_text(theme),
            _ => panel_style::muted(theme),
        });
        let title = row![mark, text(kind_label(kind)).size(panel_style::LIST_TEXT_SIZE), word].spacing(8).align_y(iced::Center);
        let mut body = column![title].spacing(6);
        if let Some(status) = status {
            body = body.push(text(status.detail.as_str()).size(12).style(panel_style::muted));
        }
        if let Some(settings) = settings {
            body = body.push(settings);
        }
        container(body).padding(10).width(Length::Fill).style(style::card).into()
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

impl Area for State {
    type Message = Message;

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Event(event) => self.event(event, ctx),
            Message::Probed(statuses) => {
                self.probing = false;
                self.statuses = Some(statuses);
                self.publish(ctx);
                Task::none()
            }
            Message::Refresh => self.probe(),
            Message::View(view) => {
                self.view = view;
                Task::none()
            }
            Message::Select(run) => self.select(run, ctx),
            Message::Diff(message) => self.diff.update(message).0.map(Message::Diff),
            Message::Apply(run) => self.apply(run, ctx),
            Message::Applied { run, result } => {
                self.applying = None;
                match result {
                    Ok(()) => {
                        self.runs.mark_applied(run);
                        if let Some(entry) = self.runs.get(run) {
                            ctx.notify(format!("Applied the sandbox patch to {}", entry.run.workspace.display()));
                        }
                    }
                    Err(error) => ctx.notify(format!("Can't apply the sandbox patch: {error}")),
                }
                Task::none()
            }
            Message::OpenWorker(worker) => {
                ctx.request(Request::OpenSession(worker));
                Task::none()
            }
            Message::Edit(field, value) => {
                *self.draft.field(field) = value;
                Task::none()
            }
            Message::Network(network) => {
                self.draft.docker_network = network;
                Task::none()
            }
            Message::Save => self.save(ctx),
            Message::PeerFailed(served) => {
                ctx.notify(format!("A sandbox run for peer {} failed on this device: {}", served.peer, served.error));
                Task::none()
            }
        }
    }

    fn palette_commands(&self, _: &Ctx) -> Vec<palette::Command> {
        vec![
            palette::Command::new(AreaId::Sandbox.command(REFRESH), "Recheck sandbox availability", "Sandbox"),
            palette::Command::new(AreaId::Sandbox.command(SETTINGS), "Sandbox settings", "Sandbox"),
        ]
    }

    fn run_command(&mut self, id: &str, ctx: &mut Ctx) -> Option<Task<Message>> {
        let name = id.strip_prefix(AreaId::Sandbox.key())?.strip_prefix('.')?;
        match name {
            REFRESH => {
                self.view = View::Settings;
                ctx.open_tab(TAB);
                Some(self.probe())
            }
            SETTINGS => {
                self.view = View::Settings;
                ctx.open_tab(TAB);
                Some(Task::none())
            }
            _ => None,
        }
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
        let adapter = ctx.services.harnesses.registry.get(target.harness)?;
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

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run_with(Events(Arc::clone(&self.events)), events).map(Message::Event),
            Subscription::run_with(Events(Arc::clone(&self.served)), events).map(Message::PeerFailed),
        ])
    }
}

/// A broadcast feed, identified by the receiver it subscribes from.
struct Events<T>(Arc<broadcast::Receiver<T>>);

impl<T> Hash for Events<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

/// Every item from now on; skips what it fell behind on.
fn events<T: Clone + Send + 'static>(events: &Events<T>) -> impl Stream<Item = T> + use<T> {
    let mut receiver = events.0.resubscribe();
    iced::stream::channel(64, async move |mut out: iced::futures::channel::mpsc::Sender<T>| {
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    if out.send(event).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}
