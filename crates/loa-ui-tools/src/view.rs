//! Rendering of the Tools panel.

use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, space, text, text_input,
    toggler,
};
use iced::{Element, Fill, Shrink};
use loa_ui_icons::{Icon, icon, muted};

use crate::catalog::{GroupView, ServerView, ToolRow};
use crate::form::FieldError;
use crate::model::{BuiltinGroup, RemoteHost, ServerKind, ServerStatus, ToolId, transport_label};
use crate::style::{self, Tone};
use crate::{Message, PeerChoice, Section, ServerAction, State, TransportKind};

const SPACING: f32 = 8.0;
const SMALL: f32 = 12.0;
/// Primary text: names and field values.
const BODY: f32 = 14.0;
/// Notes and empty states.
const NOTE: f32 = 13.0;
/// Gap between whole blocks (groups, servers, hosts, tabs).
const BLOCK_SPACING: f32 = 16.0;
/// Size of icons drawn next to small text.
const ICON: f32 = 12.0;

impl State {
    pub fn view(&self) -> Element<'_, Message> {
        let tabs = row([
            (Section::Builtin, "Built in"),
            (Section::Servers, "MCP servers"),
            (Section::Hosts, "Remote hosts"),
            (Section::Peers, "Peers"),
        ]
        .into_iter()
        .map(|(section, label)| tab(label, section, self.section)))
        .spacing(BLOCK_SPACING);

        let filter = search("Filter tools", &self.filter_text, Message::FilterChanged);

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

        let body = scrollable(body)
            .direction(style::thin_scrollbar())
            .style(style::scrollbar)
            .height(Fill);
        column![header, style::divider(), body]
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
            text("Project").size(SMALL).style(style::muted),
            pick_list(self.projects.as_slice(), selected, Message::ProjectSelected)
                .placeholder("Choose a project")
                .text_size(NOTE)
                .padding([4.0, 8.0])
                .style(style::pick),
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
            content = content.push(note("No built in tool matches the filter"));
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
        let mut content = column![style::section_header(title)].spacing(SPACING);
        if group.tools.is_empty() {
            content = content.push(note("No tools"));
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
                .size(14)
                .text_size(BODY)
                .spacing(SPACING)
                .on_toggle_maybe(on_toggle),
            text(info.description.as_str())
                .size(SMALL)
                .style(style::muted),
        ]
        .spacing(2)
        .into()
    }

    fn servers_view(&self) -> Element<'_, Message> {
        let actions = row![
            flat(with_icon(Icon::Plus, "Add server")).on_press(Message::AddServerOpen),
            flat(text("Import").size(SMALL)).on_press(Message::ImportScan),
        ]
        .spacing(4);
        let mut content = column![actions].spacing(BLOCK_SPACING);
        if let Some(form) = self.add_server_form() {
            content = content.push(form);
        }
        if let Some(imports) = self.import_view() {
            content = content.push(imports);
        }
        let catalog = self.catalog();
        if catalog.servers.is_empty() {
            let note_text = if self.filter.is_empty() {
                "No MCP servers"
            } else {
                "No server matches the filter"
            };
            content = content.push(note(note_text));
        }
        for server in catalog.servers {
            content = content.push(self.server_view(server));
        }
        content.into()
    }

    fn server_view<'a>(&'a self, view: ServerView<'a>) -> Element<'a, Message> {
        let server = view.server;
        let name = server.name.as_str();
        let (tone, status_label) = match &server.status {
            ServerStatus::Running => (Tone::Ok, "running".to_owned()),
            ServerStatus::Stopped => (Tone::Off, "stopped".to_owned()),
            ServerStatus::Error(message) => (Tone::Bad, format!("error: {message}")),
        };
        let mut title = row![
            container(space().width(6).height(6)).style(style::dot(tone)),
            text(name).size(BODY),
        ]
        .spacing(8)
        .align_y(iced::Center)
        .width(Fill);
        match server.kind {
            ServerKind::Surface => {
                title = title.push(text("surface").size(SMALL).style(style::muted));
            }
            ServerKind::AppHosted => {
                title = title.push(text("app").size(SMALL).style(style::muted));
            }
            ServerKind::External => {}
        }
        title = title
            .push(space().width(Fill))
            .push(text(status_label).size(SMALL).style(style::tone_text(tone)));

        let action = |label: &'a str, action: ServerAction, enabled: bool| {
            flat(text(label).size(SMALL))
                .on_press_maybe(enabled.then(|| Message::Server(name.to_owned(), action)))
        };
        let running = server.status == ServerStatus::Running;
        let toggle = flat(text(format!("Tools: {}", view.toggle.label())).size(SMALL)).on_press_maybe(
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
                flat(text("Remove").size(SMALL))
                    .style(style::danger_flat)
                    .on_press(Message::Server(name.to_owned(), ServerAction::Remove)),
            );
        }

        let mut tools = column![].spacing(SPACING);
        if view.tools.is_empty() {
            tools = tools.push(note("No tools declared"));
        }
        for tool in view.tools {
            tools = tools.push(self.tool_row(tool));
        }

        list_item(
            column![
                title,
                text(transport_label(&server.transport))
                    .size(SMALL)
                    .style(style::muted),
                actions,
                container(tools).padding(iced::Padding::ZERO.left(16)),
            ]
            .spacing(SPACING),
        )
    }

    fn add_server_form(&self) -> Option<Element<'_, Message>> {
        let (form, errors) = self.add_server.as_ref()?;
        let kind = |label: &'static str, kind: TransportKind| {
            button(text(label).size(SMALL))
                .padding([4.0, 8.0])
                .style(if form.kind == kind {
                    style::selected
                } else {
                    style::flat
                })
                .on_press(Message::AddServerKind(kind))
        };
        let target = match form.kind {
            TransportKind::Stdio => field(
                "Command, e.g. npx -y @scope/server",
                &form.command,
                Message::AddServerCommand,
            )
            .on_submit(Message::AddServerSubmit),
            TransportKind::Http => field("https://host/mcp", &form.url, Message::AddServerUrl)
                .on_submit(Message::AddServerSubmit),
        };
        let content = column![
            style::section_header("Add server"),
            field("Name", &form.name, Message::AddServerName)
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
                filled("Add").on_press(Message::AddServerSubmit),
                flat(text("Cancel").size(SMALL)).on_press(Message::AddServerCancel),
            ]
            .spacing(SPACING),
        ]
        .spacing(6);
        Some(
            container(content)
                .padding(SPACING)
                .width(Fill)
                .style(style::card)
                .into(),
        )
    }

    fn import_view(&self) -> Option<Element<'_, Message>> {
        let entries = self.imports.as_ref()?;
        let mut content =
            column![style::section_header("Import from .claude, .cursor, .codex")].spacing(6);
        if self.import_scanning {
            content = content.push(note("Scanning..."));
        } else if entries.is_empty() {
            content = content.push(note("No server configs found"));
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
                    .size(14)
                    .text_size(NOTE)
                    .on_toggle(move |on| Message::ImportSelect(index, on))
            ]
            .spacing(SPACING)
            .align_y(iced::Center);
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
                    .style(style::muted),
                Some(crate::ImportOutcome::Failed(why)) => text(format!("failed: {why}"))
                    .size(SMALL)
                    .style(text::danger),
            });
            content = content.push(line);
        }
        let any_selected = entries.iter().any(|e| e.selected);
        content = content.push(
            row![
                filled("Import selected")
                    .on_press_maybe(any_selected.then_some(Message::ImportSubmit)),
                flat(with_icon(Icon::X, "Close")).on_press(Message::ImportClose),
            ]
            .spacing(SPACING)
            .align_y(iced::Center),
        );
        Some(
            container(content)
                .padding(SPACING)
                .width(Fill)
                .style(style::card)
                .into(),
        )
    }

    fn hosts_view(&self) -> Element<'_, Message> {
        let actions = row![
            flat(text("Import from ssh config").size(SMALL)).on_press(Message::ImportSshHosts),
            flat(with_icon(Icon::Plus, "Add host")).on_press(Message::AddHostOpen),
        ]
        .spacing(4);
        let mut content = column![actions].spacing(BLOCK_SPACING);
        if let Some((form, errors)) = &self.add_host {
            let form = column![
                style::section_header("Add host"),
                field("Alias", &form.alias, Message::AddHostAlias),
                field_error(errors.alias.as_ref()),
                field("Hostname", &form.hostname, Message::AddHostHostname),
                field_error(errors.hostname.as_ref()),
                field("User (optional)", &form.user, Message::AddHostUser),
                field_error(errors.user.as_ref()),
                field("Port (optional)", &form.port, Message::AddHostPort),
                field_error(errors.port.as_ref()),
                row![
                    filled("Add").on_press(Message::AddHostSubmit),
                    flat(text("Cancel").size(SMALL)).on_press(Message::AddHostCancel),
                ]
                .spacing(SPACING),
            ]
            .spacing(6);
            content = content.push(
                container(form)
                    .padding(SPACING)
                    .width(Fill)
                    .style(style::card),
            );
        }
        let hosts: Vec<_> = self
            .hosts
            .iter()
            .filter(|h| self.filter.matches(&h.alias) || self.filter.matches(&h.hostname))
            .collect();
        if hosts.is_empty() {
            content = content.push(note("No remote hosts"));
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
            flat(text("Edit limits").size(SMALL)).on_press(Message::EditLimits(alias.to_owned())),
            flat(text("Terminal").size(SMALL)).on_press(Message::OpenTerminal(alias.to_owned())),
            flat(text("Audit log").size(SMALL)).on_press(Message::ShowAudit(alias.to_owned())),
            flat(text("Remove").size(SMALL))
                .style(style::danger_flat)
                .on_press(Message::RemoveHost(alias.to_owned())),
        ]
        .spacing(4);
        let mut content = column![
            text(alias).size(BODY),
            text(target).size(SMALL).style(style::muted),
            text(summary).size(SMALL).style(style::muted),
            actions,
        ]
        .spacing(4);
        if let Some(draft) = self.limits.as_ref().filter(|d| d.host == host.alias) {
            let mut paths = column![].spacing(2);
            for (index, path) in draft.limits.allowed_paths.iter().enumerate() {
                paths = paths.push(
                    row![
                        text(path.as_str())
                            .size(SMALL)
                            .font(iced::Font::MONOSPACE)
                            .width(Fill),
                        flat(text("Remove").size(SMALL))
                            .style(style::danger_flat)
                            .on_press(Message::LimitsRemovePath(index)),
                    ]
                    .align_y(iced::Center),
                );
            }
            let editor = column![
                checkbox(draft.limits.read_only)
                    .label("Read only")
                    .size(14)
                    .text_size(NOTE)
                    .on_toggle(Message::LimitsReadOnly),
                checkbox(draft.limits.commands_allowed)
                    .label("Commands allowed")
                    .size(14)
                    .text_size(NOTE)
                    .on_toggle(Message::LimitsCommands),
                style::section_header("Allowed paths"),
                paths,
                row![
                    field(
                        "/path or ~/path",
                        &draft.new_path,
                        Message::LimitsNewPath,
                    )
                    .on_submit(Message::LimitsAddPath),
                    flat(with_icon(Icon::Plus, "Add path")).on_press(Message::LimitsAddPath),
                ]
                .spacing(4)
                .align_y(iced::Center),
                field_error(draft.path_error.as_ref()),
                row![
                    filled("Save").on_press(Message::LimitsSave),
                    flat(text("Cancel").size(SMALL)).on_press(Message::LimitsCancel),
                ]
                .spacing(SPACING),
            ]
            .spacing(6);
            content = content.push(
                container(editor)
                    .padding(SPACING)
                    .width(Fill)
                    .style(style::card),
            );
        }
        if let Some((_, rows)) = self.audit.as_ref().filter(|(h, _)| h == alias) {
            let mut log = column![
                row![
                    style::section_header("Audit log"),
                    space().width(Fill),
                    flat(with_icon(Icon::X, "Close")).on_press(Message::CloseAudit),
                ]
                .align_y(iced::Center)
            ]
            .spacing(2);
            if rows.is_empty() {
                log = log.push(text("No entries").size(SMALL).style(style::muted));
            }
            for entry in rows {
                log = log.push(
                    row![
                        text(entry.when.as_str())
                            .size(SMALL)
                            .style(style::muted)
                            .width(160),
                        text(entry.action.label()).size(SMALL).width(70),
                        text(entry.detail.as_str()).size(SMALL).width(Fill),
                        text(entry.session.as_str())
                            .size(SMALL)
                            .style(style::muted),
                    ]
                    .spacing(SPACING),
                );
            }
            content = content.push(container(log).padding(iced::Padding::ZERO.left(16)));
        }
        list_item(content)
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
            text("Show").size(SMALL).style(style::muted),
            pick_list(choices, selected, Message::PeerFilter)
                .text_size(NOTE)
                .padding([4.0, 8.0])
                .style(style::pick),
        ]
        .spacing(SPACING)
        .align_y(iced::Center);
        let mut content = column![
            selector,
            text("Read only: tools each peer's grant allows")
                .size(SMALL)
                .style(style::muted),
        ]
        .spacing(SPACING);

        let matrix = self.peer_matrix();
        if matrix.peers.is_empty() {
            return content
                .push(note("No peers"))
                .into();
        }
        let mut header = row![text("Tool").size(SMALL).style(style::muted).width(Fill)]
            .spacing(SPACING);
        for peer in &matrix.peers {
            header = header.push(
                text(peer.name.as_str())
                    .size(SMALL)
                    .style(style::muted)
                    .width(80),
            );
        }
        let mut table = column![header, style::divider()].spacing(4);
        if matrix.rows.is_empty() {
            table = table.push(note("No tools"));
        }
        for line in matrix.rows {
            let label = match &line.tool {
                ToolId::Builtin(name) => name.clone(),
                ToolId::Mcp { server, tool } => format!("{server} / {tool}"),
            };
            let mut cells = row![text(label).size(SMALL).width(Fill)].spacing(SPACING);
            for allowed in line.allowed {
                cells = cells.push(if allowed {
                    text("allowed").size(SMALL).width(80).style(text::success)
                } else {
                    text("no").size(SMALL).width(80).style(style::muted)
                });
            }
            table = table.push(cells);
        }
        content = content.push(table);
        content.into()
    }
}

/// A panel tab: bright over a 2px underline when active, muted otherwise.
fn tab(label: &str, section: Section, current: Section) -> Element<'_, Message> {
    let active = section == current;
    button(
        column![
            text(label)
                .size(13)
                .wrapping(text::Wrapping::None)
                .style(if active { text::base } else { style::muted }),
            container(space().height(1))
                .width(Fill)
                .style(style::underline(active)),
        ]
        .width(Shrink)
        .spacing(6)
        .align_x(iced::Center),
    )
    .padding([4.0, 0.0])
    .style(style::plain)
    .on_press(Message::SectionSelected(section))
    .into()
}

/// A small flat button: transparent until hovered.
fn flat<'a>(content: impl Into<Element<'a, Message>>) -> button::Button<'a, Message> {
    button(content).padding([4.0, 8.0]).style(style::flat)
}

/// The one filled action of a form: small and neutral.
fn filled(label: &str) -> button::Button<'_, Message> {
    button(text(label).size(SMALL))
        .padding([4.0, 10.0])
        .style(button::primary)
}

/// A flat button with a leading icon.
fn with_icon(glyph: Icon, label: &str) -> Element<'_, Message> {
    row![icon(glyph, ICON), text(label).size(SMALL)]
        .spacing(6)
        .align_y(iced::Center)
        .into()
}

/// A raised text field, styled like every other input in the panel.
fn field<'a>(
    placeholder: &'a str,
    value: &'a str,
    on_input: impl Fn(String) -> Message + 'a,
) -> text_input::TextInput<'a, Message> {
    text_input(placeholder, value)
        .on_input(on_input)
        .size(BODY)
        .style(style::input)
        .padding([6.0, 8.0])
}

/// A raised search box with a magnifier icon and a borderless field.
fn search<'a>(
    placeholder: &'a str,
    value: &'a str,
    on_input: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    container(
        row![
            muted(Icon::Search, 14.0),
            text_input(placeholder, value)
                .on_input(on_input)
                .size(BODY)
                .style(style::bare)
                .padding(0.0),
        ]
        .spacing(8)
        .align_y(iced::Center),
    )
    .padding([6.0, 10.0])
    .width(Fill)
    .style(style::card)
    .into()
}

/// One entry of a list: its content, then a divider.
fn list_item<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    column![
        container(content).padding([10.0, 0.0]).width(Fill),
        style::divider(),
    ]
    .into()
}

/// A muted line for empty states and progress.
fn note(label: &str) -> text::Text<'_> {
    text(label).size(NOTE).style(style::muted)
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
