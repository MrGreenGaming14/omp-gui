//! Rendering of the Tools panel.

use iced::widget::{
    button, checkbox, column, container, pick_list, row, rule, scrollable, space, text, text_input,
    toggler,
};
use iced::{Element, Fill};

use crate::catalog::{GroupView, ServerView, ToolRow};
use crate::form::FieldError;
use crate::model::{BuiltinGroup, RemoteHost, ServerKind, ServerStatus, ToolId, transport_label};
use crate::{Message, PeerChoice, Section, ServerAction, State, TransportKind};

const SPACING: f32 = 8.0;
const SMALL: f32 = 12.0;
/// Gap between whole blocks (groups, servers, hosts).
const BLOCK_SPACING: f32 = 16.0;

impl State {
    pub fn view(&self) -> Element<'_, Message> {
        let tabs = row([
            (Section::Builtin, "Built in"),
            (Section::Servers, "MCP servers"),
            (Section::Hosts, "Remote hosts"),
            (Section::Peers, "Peers"),
        ]
        .into_iter()
        .map(|(section, label)| tab(label, section, self.section).into()))
        .spacing(4);

        let filter = text_input("Filter tools", &self.filter_text).on_input(Message::FilterChanged);

        let mut header = column![tabs, filter].spacing(SPACING);
        if matches!(self.section, Section::Builtin | Section::Servers) {
            header = header.push(self.project_selector());
        }

        let body = match self.section {
            Section::Builtin => self.builtin_view(),
            Section::Servers => self.servers_view(),
            Section::Hosts => self.hosts_view(),
            Section::Peers => self.peers_view(),
        };

        column![header, rule::horizontal(1), scrollable(body).height(Fill)]
            .spacing(SPACING)
            .padding(SPACING)
            .into()
    }

    fn project_selector(&self) -> Element<'_, Message> {
        let selected = self
            .project
            .as_ref()
            .and_then(|id| self.projects.iter().find(|p| &p.id == id));
        row![
            text("Project"),
            pick_list(self.projects.as_slice(), selected, Message::ProjectSelected)
                .placeholder("Choose a project"),
        ]
        .spacing(SPACING)
        .align_y(iced::Center)
        .into()
    }

    fn can_toggle(&self) -> bool {
        self.project.is_some()
    }

    fn builtin_view(&self) -> Element<'_, Message> {
        let catalog = self.catalog();
        let mut content = column![].spacing(BLOCK_SPACING);
        if catalog.builtin.is_empty() {
            content =
                content.push(text("No built in tool matches the filter").style(text::secondary));
        }
        for group in catalog.builtin {
            content = content.push(self.group_view(group));
        }
        content.into()
    }

    fn group_view<'a>(&'a self, group: GroupView<'a>) -> Element<'a, Message> {
        let title = match group.group {
            BuiltinGroup::Harness if group.title.is_empty() => "Harness tools".to_owned(),
            BuiltinGroup::Harness => format!("Harness tools ({})", group.title),
            BuiltinGroup::App => group.title.to_owned(),
        };
        let mut content = column![text(title).size(16)].spacing(SPACING);
        if group.tools.is_empty() {
            content = content.push(text("No tools").style(text::secondary));
        }
        for tool in group.tools {
            content = content.push(self.tool_row(tool));
        }
        content.into()
    }

    fn tool_row<'a>(&'a self, tool: ToolRow<'a>) -> Element<'a, Message> {
        let ToolRow { id, info, enabled } = tool;
        let on_toggle = self
            .can_toggle()
            .then_some(move |on| Message::ToolToggled(id.clone(), on));
        column![
            toggler(enabled)
                .label(info.name.as_str())
                .on_toggle_maybe(on_toggle),
            text(info.description.as_str())
                .size(SMALL)
                .style(text::secondary),
        ]
        .spacing(2)
        .into()
    }

    fn servers_view(&self) -> Element<'_, Message> {
        let actions = row![
            button("Add server").on_press(Message::AddServerOpen),
            button("Import")
                .on_press(Message::ImportScan)
                .style(button::secondary),
        ]
        .spacing(SPACING);
        let mut content = column![actions].spacing(BLOCK_SPACING);
        if let Some(form) = self.add_server_form() {
            content = content.push(form);
        }
        if let Some(imports) = self.import_view() {
            content = content.push(imports);
        }
        let catalog = self.catalog();
        if catalog.servers.is_empty() {
            let note = if self.filter.is_empty() {
                "No MCP servers"
            } else {
                "No server matches the filter"
            };
            content = content.push(text(note).style(text::secondary));
        }
        for server in catalog.servers {
            content = content.push(self.server_view(server));
        }
        content.into()
    }

    fn server_view<'a>(&'a self, view: ServerView<'a>) -> Element<'a, Message> {
        let server = view.server;
        let name = server.name.as_str();
        let mut title = row![text(name).size(16)]
            .spacing(SPACING)
            .align_y(iced::Center);
        match server.kind {
            ServerKind::Surface => {
                title = title.push(text("surface").size(SMALL).style(text::primary))
            }
            ServerKind::AppHosted => {
                title = title.push(text("app").size(SMALL).style(text::primary))
            }
            ServerKind::External => {}
        }
        let status: Element<'_, Message> = match &server.status {
            ServerStatus::Running => text("running").style(text::success).into(),
            ServerStatus::Stopped => text("stopped").style(text::secondary).into(),
            ServerStatus::Error(message) => {
                text(format!("error: {message}")).style(text::danger).into()
            }
        };
        title = title.push(status);

        let action = |label: &'a str, action: ServerAction, enabled: bool| {
            button(text(label).size(SMALL))
                .style(button::secondary)
                .on_press_maybe(enabled.then(|| Message::Server(name.to_owned(), action)))
        };
        let running = server.status == ServerStatus::Running;
        let toggle = button(text(format!("Tools: {}", view.toggle.label())).size(SMALL))
            .style(button::secondary)
            .on_press_maybe(
                self.can_toggle()
                    .then(|| Message::ServerToggled(name.to_owned())),
            );
        let mut actions = row![
            toggle,
            action("Start", ServerAction::Start, !running),
            action("Stop", ServerAction::Stop, running),
            action("Restart", ServerAction::Restart, running),
        ]
        .spacing(4);
        if server.can_remove() {
            actions = actions.push(
                button(text("Remove").size(SMALL))
                    .style(button::danger)
                    .on_press(Message::Server(name.to_owned(), ServerAction::Remove)),
            );
        }

        let mut tools = column![].spacing(SPACING);
        if view.tools.is_empty() {
            tools = tools.push(text("No tools declared").style(text::secondary));
        }
        for tool in view.tools {
            tools = tools.push(self.tool_row(tool));
        }

        container(
            column![
                title,
                text(transport_label(&server.transport))
                    .size(SMALL)
                    .style(text::secondary),
                actions,
                container(tools).padding(iced::Padding::ZERO.left(16)),
            ]
            .spacing(SPACING),
        )
        .padding(SPACING)
        .width(Fill)
        .style(container::bordered_box)
        .into()
    }

    fn add_server_form(&self) -> Option<Element<'_, Message>> {
        let (form, errors) = self.add_server.as_ref()?;
        let kind = |label: &'static str, kind: TransportKind| {
            button(text(label).size(SMALL))
                .style(if form.kind == kind {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press(Message::AddServerKind(kind))
        };
        let target = match form.kind {
            TransportKind::Stdio => text_input("Command, e.g. npx -y @scope/server", &form.command)
                .on_input(Message::AddServerCommand)
                .on_submit(Message::AddServerSubmit),
            TransportKind::Http => text_input("https://host/mcp", &form.url)
                .on_input(Message::AddServerUrl)
                .on_submit(Message::AddServerSubmit),
        };
        let content = column![
            text("Add server").size(16),
            text_input("Name", &form.name)
                .on_input(Message::AddServerName)
                .on_submit(Message::AddServerSubmit),
            field_error(errors.name.as_ref()),
            row![
                kind("stdio", TransportKind::Stdio),
                kind("HTTP", TransportKind::Http)
            ]
            .spacing(4),
            target,
            field_error(errors.target.as_ref()),
            row![
                button("Add").on_press(Message::AddServerSubmit),
                button("Cancel")
                    .style(button::secondary)
                    .on_press(Message::AddServerCancel),
            ]
            .spacing(SPACING),
        ]
        .spacing(4);
        Some(
            container(content)
                .padding(SPACING)
                .width(Fill)
                .style(container::bordered_box)
                .into(),
        )
    }

    fn import_view(&self) -> Option<Element<'_, Message>> {
        let entries = self.imports.as_ref()?;
        let mut content = column![text("Import from .claude, .cursor, .codex").size(16)].spacing(4);
        if self.import_scanning {
            content = content.push(text("Scanning...").style(text::secondary));
        } else if entries.is_empty() {
            content = content.push(text("No server configs found").style(text::secondary));
        }
        for (index, entry) in entries.iter().enumerate() {
            let candidate = &entry.candidate;
            let label = format!(
                "{}  ({} {})",
                candidate.server.name,
                candidate.source.label(),
                candidate.path
            );
            let mut line = row![
                checkbox(entry.selected)
                    .label(label)
                    .on_toggle(move |on| Message::ImportSelect(index, on))
            ]
            .spacing(SPACING);
            if entry.exists {
                line = line.push(text("already registered").size(SMALL).style(text::warning));
            }
            line = line.push(match &entry.outcome {
                None => text(""),
                Some(crate::ImportOutcome::Imported) => {
                    text("imported").size(SMALL).style(text::success)
                }
                Some(crate::ImportOutcome::Skipped(why)) => text(format!("skipped: {why}"))
                    .size(SMALL)
                    .style(text::secondary),
                Some(crate::ImportOutcome::Failed(why)) => text(format!("failed: {why}"))
                    .size(SMALL)
                    .style(text::danger),
            });
            content = content.push(line);
        }
        let any_selected = entries.iter().any(|e| e.selected);
        content = content.push(
            row![
                button("Import selected")
                    .on_press_maybe(any_selected.then_some(Message::ImportSubmit)),
                button("Close")
                    .style(button::secondary)
                    .on_press(Message::ImportClose),
            ]
            .spacing(SPACING),
        );
        Some(
            container(content)
                .padding(SPACING)
                .width(Fill)
                .style(container::bordered_box)
                .into(),
        )
    }

    fn hosts_view(&self) -> Element<'_, Message> {
        let actions = row![
            button("Import from ssh config").on_press(Message::ImportSshHosts),
            button("Add host")
                .style(button::secondary)
                .on_press(Message::AddHostOpen),
        ]
        .spacing(SPACING);
        let mut content = column![actions].spacing(BLOCK_SPACING);
        if let Some((form, errors)) = &self.add_host {
            let form = column![
                text("Add host").size(16),
                text_input("Alias", &form.alias).on_input(Message::AddHostAlias),
                field_error(errors.alias.as_ref()),
                text_input("Hostname", &form.hostname).on_input(Message::AddHostHostname),
                field_error(errors.hostname.as_ref()),
                text_input("User (optional)", &form.user).on_input(Message::AddHostUser),
                field_error(errors.user.as_ref()),
                text_input("Port (optional)", &form.port).on_input(Message::AddHostPort),
                field_error(errors.port.as_ref()),
                row![
                    button("Add").on_press(Message::AddHostSubmit),
                    button("Cancel")
                        .style(button::secondary)
                        .on_press(Message::AddHostCancel),
                ]
                .spacing(SPACING),
            ]
            .spacing(4);
            content = content.push(
                container(form)
                    .padding(SPACING)
                    .width(Fill)
                    .style(container::bordered_box),
            );
        }
        let hosts: Vec<_> = self
            .hosts
            .iter()
            .filter(|h| self.filter.matches(&h.alias) || self.filter.matches(&h.hostname))
            .collect();
        if hosts.is_empty() {
            content = content.push(text("No remote hosts").style(text::secondary));
        }
        for host in hosts {
            content = content.push(self.host_view(host));
        }
        content.into()
    }

    fn host_view<'a>(&'a self, host: &'a RemoteHost) -> Element<'a, Message> {
        let alias = host.alias.as_str();
        let mut target = String::new();
        if let Some(user) = &host.user {
            target.push_str(user);
            target.push('@');
        }
        target.push_str(&host.hostname);
        if let Some(port) = host.port {
            target.push(':');
            target.push_str(&port.to_string());
        }
        if let Some(jump) = &host.proxy_jump {
            target.push_str(" via ");
            target.push_str(jump);
        }
        let limits = &host.limits;
        let paths = if limits.allowed_paths.is_empty() {
            "no paths".to_owned()
        } else {
            limits.allowed_paths.join(", ")
        };
        let summary = format!(
            "{}, {}, {}",
            if limits.read_only {
                "read only"
            } else {
                "read write"
            },
            if limits.commands_allowed {
                "commands allowed"
            } else {
                "no commands"
            },
            paths,
        );
        let actions = row![
            button(text("Edit limits").size(SMALL))
                .style(button::secondary)
                .on_press(Message::EditLimits(alias.to_owned())),
            button(text("Terminal").size(SMALL))
                .style(button::secondary)
                .on_press(Message::OpenTerminal(alias.to_owned())),
            button(text("Audit log").size(SMALL))
                .style(button::secondary)
                .on_press(Message::ShowAudit(alias.to_owned())),
            button(text("Remove").size(SMALL))
                .style(button::danger)
                .on_press(Message::RemoveHost(alias.to_owned())),
        ]
        .spacing(4);
        let mut content = column![
            text(alias).size(16),
            text(target).size(SMALL).style(text::secondary),
            text(summary).size(SMALL),
            actions,
        ]
        .spacing(4);
        if let Some(draft) = self.limits.as_ref().filter(|d| d.host == host.alias) {
            let mut paths = column![].spacing(2);
            for (index, path) in draft.limits.allowed_paths.iter().enumerate() {
                paths = paths.push(
                    row![
                        text(path.as_str()).width(Fill),
                        button(text("Remove").size(SMALL))
                            .style(button::text)
                            .on_press(Message::LimitsRemovePath(index)),
                    ]
                    .align_y(iced::Center),
                );
            }
            let editor = column![
                checkbox(draft.limits.read_only)
                    .label("Read only")
                    .on_toggle(Message::LimitsReadOnly),
                checkbox(draft.limits.commands_allowed)
                    .label("Commands allowed")
                    .on_toggle(Message::LimitsCommands),
                text("Allowed paths").size(SMALL),
                paths,
                row![
                    text_input("/path or ~/path", &draft.new_path)
                        .on_input(Message::LimitsNewPath)
                        .on_submit(Message::LimitsAddPath),
                    button("Add path").on_press(Message::LimitsAddPath),
                ]
                .spacing(4),
                field_error(draft.path_error.as_ref()),
                row![
                    button("Save").on_press(Message::LimitsSave),
                    button("Cancel")
                        .style(button::secondary)
                        .on_press(Message::LimitsCancel),
                ]
                .spacing(SPACING),
            ]
            .spacing(4);
            content = content.push(container(editor).padding(iced::Padding::ZERO.left(16)));
        }
        if let Some((_, rows)) = self.audit.as_ref().filter(|(h, _)| h == alias) {
            let mut log = column![
                row![
                    text("Audit log").size(SMALL),
                    space().width(Fill),
                    button(text("Close").size(SMALL))
                        .style(button::text)
                        .on_press(Message::CloseAudit)
                ]
                .align_y(iced::Center)
            ]
            .spacing(2);
            if rows.is_empty() {
                log = log.push(text("No entries").size(SMALL).style(text::secondary));
            }
            for entry in rows {
                log = log.push(
                    row![
                        text(entry.when.as_str()).size(SMALL).width(160),
                        text(entry.action.label()).size(SMALL).width(70),
                        text(entry.detail.as_str()).size(SMALL).width(Fill),
                        text(entry.session.as_str())
                            .size(SMALL)
                            .style(text::secondary),
                    ]
                    .spacing(SPACING),
                );
            }
            content = content.push(container(log).padding(iced::Padding::ZERO.left(16)));
        }
        container(content)
            .padding(SPACING)
            .width(Fill)
            .style(container::bordered_box)
            .into()
    }

    fn peers_view(&self) -> Element<'_, Message> {
        let choices: Vec<PeerChoice> = std::iter::once(PeerChoice::All)
            .chain(self.peers.iter().map(|p| PeerChoice::Peer {
                key: p.key.clone(),
                name: p.name.clone(),
            }))
            .collect();
        let selected = match &self.peer_filter {
            None => Some(PeerChoice::All),
            Some(key) => choices
                .iter()
                .find(|c| matches!(c, PeerChoice::Peer { key: k, .. } if k == key))
                .cloned(),
        };
        let selector = row![
            text("Show"),
            pick_list(choices, selected, Message::PeerFilter)
        ]
        .spacing(SPACING)
        .align_y(iced::Center);
        let mut content = column![
            selector,
            text("Read only: tools each peer's grant allows")
                .size(SMALL)
                .style(text::secondary)
        ]
        .spacing(SPACING);

        let matrix = self.peer_matrix();
        if matrix.peers.is_empty() {
            return content.push(text("No peers").style(text::secondary)).into();
        }
        let mut header = row![text("Tool").width(220)].spacing(SPACING);
        for peer in &matrix.peers {
            header = header.push(text(peer.name.as_str()).width(80));
        }
        let mut table = column![header, rule::horizontal(1)].spacing(4);
        if matrix.rows.is_empty() {
            table = table.push(text("No tools").style(text::secondary));
        }
        for line in matrix.rows {
            let label = match &line.tool {
                ToolId::Builtin(name) => name.clone(),
                ToolId::Mcp { server, tool } => format!("{server} / {tool}"),
            };
            let mut cells = row![text(label).width(220)].spacing(SPACING);
            for allowed in line.allowed {
                cells = cells.push(if allowed {
                    text("allowed").width(80).style(text::success)
                } else {
                    text("no").width(80).style(text::secondary)
                });
            }
            table = table.push(cells);
        }
        content = content.push(table);
        content.into()
    }
}

fn tab(label: &str, section: Section, current: Section) -> iced::widget::Button<'_, Message> {
    button(text(label))
        .style(if section == current {
            button::primary
        } else {
            button::secondary
        })
        .on_press(Message::SectionSelected(section))
}

fn field_error(error: Option<&FieldError>) -> Element<'_, Message> {
    match error {
        Some(error) => text(error.to_string())
            .size(SMALL)
            .style(text::danger)
            .into(),
        None => space().height(0).into(),
    }
}
