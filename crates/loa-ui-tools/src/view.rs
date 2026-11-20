//! Rendering of the Tools panel.
//!
//! Every list sits in a bordered card with a divider between its rows and a
//! small header above it that names the list and counts it. Rows are one
//! line: a checkbox and a name on the left, a clipped description, metadata
//! on the right. Long lists show their first rows until opened, and a tool's
//! details open inline under its row.

use iced::widget::{
    button, checkbox, column, container, pick_list, responsive, row, scrollable, space, text,
    text_input,
};
use iced::{Alignment, Element, Fill, Font, Length, Padding, Shrink, font};
use loa_ui_icons::{Icon, icon, muted};

use crate::catalog::{Catalog, GroupView, ServerView, ToolRow};
use crate::form::FieldError;
use crate::model::{
    AuditRow, BuiltinGroup, DetailState, PromptAnswer, PromptKind, RemoteHost, RemotePrompt,
    ServerKind, ServerStatus, ToolId, ToolInfo, transport_label,
};
use crate::style::{self, Tone};
use crate::{Message, PeerChoice, Section, ServerAction, State, TransportKind};

const SPACING: f32 = 8.0;
/// Descriptions, metadata and buttons.
const SMALL: f32 = 12.0;
/// Names and field values.
const BODY: f32 = 13.0;
/// Notes and empty states.
const NOTE: f32 = 13.0;
/// Gap between cards.
const GAP: f32 = 16.0;
/// Size of icons drawn next to small text.
const ICON: f32 = 12.0;
/// Inner padding of a card around its rows.
const CARD_PAD: f32 = 4.0;
/// Padding of one row inside a card.
const ROW_PAD: Padding = Padding { top: 7.0, right: 10.0, bottom: 7.0, left: 10.0 };
/// Padding of forms and question cards.
const FORM_PAD: f32 = 12.0;
/// From this width, cards sit in two columns.
const TWO_COLUMNS: f32 = 640.0;
/// Lists longer than this show only their first rows until opened.
const COLLAPSE_AT: usize = 8;
/// Rows a collapsed list shows.
const PREVIEW: usize = 6;
/// Height of the audit log's own scroll area.
const AUDIT_HEIGHT: f32 = 260.0;
/// Width of a peer's column in the peers table.
const PEER_COLUMN: f32 = 80.0;
/// Width of a detail's label in an open tool.
const DETAIL_LABEL: f32 = 110.0;
/// Left inset of an open tool's details: under its name, past the checkbox.
const DETAIL_INSET: f32 = 34.0;

const BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..Font::DEFAULT
};

impl State {
    pub fn view(&self) -> Element<'_, Message> {
        responsive(move |size| self.layout(size.width >= TWO_COLUMNS)).into()
    }

    fn layout(&self, wide: bool) -> Element<'_, Message> {
        let catalog = self.catalog();
        let builtin_count: usize = catalog.builtin.iter().map(|g| g.tools.len()).sum();
        let hosts_label = match self.prompts.len() {
            0 => "Remote hosts".to_owned(),
            n => format!("Remote hosts ({n})"),
        };
        let tabs = row([
            (Section::Builtin, "Built in".to_owned(), builtin_count),
            (Section::Servers, "MCP servers".to_owned(), catalog.servers.len()),
            (Section::Hosts, hosts_label, self.visible_hosts().len()),
            (Section::Peers, "Peers".to_owned(), self.peers.len()),
        ]
        .into_iter()
        .map(|(section, label, count)| tab(label, count, section, self.section)))
        .spacing(GAP);
        let tabs = scrollable(tabs)
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(2).scroller_width(2),
            ))
            .style(style::scrollbar);

        let mut toolbar = row![search("Filter tools, servers and hosts", &self.filter_text, Message::FilterChanged)]
            .spacing(SPACING)
            .align_y(Alignment::Center);
        let toggles = matches!(self.section, Section::Builtin | Section::Servers);
        if toggles {
            toolbar = toolbar.push(self.project_selector());
        }
        let mut header = column![tabs, toolbar].spacing(10);
        if toggles && !self.can_toggle() {
            header = header.push(
                text("Choose a project to switch tools on or off")
                    .size(SMALL)
                    .style(style::faint),
            );
        }

        let body = match self.section {
            Section::Builtin => self.builtin_view(catalog, wide),
            Section::Servers => self.servers_view(catalog, wide),
            Section::Hosts => self.hosts_view(wide),
            Section::Peers => self.peers_view(),
        };
        let body = scrollable(container(body).padding(Padding::ZERO.top(4).bottom(12).right(10)))
            .direction(style::thin_scrollbar())
            .style(style::scrollbar)
            .height(Fill);

        column![
            container(header).padding(Padding::ZERO.right(10)),
            body
        ]
        .spacing(12)
        .padding(Padding { top: 10.0, right: 2.0, bottom: 0.0, left: 12.0 })
        .into()
    }

    fn project_selector(&self) -> Element<'_, Message> {
        let selected = self
            .project
            .as_ref()
            .and_then(|id| self.projects.iter().find(|p| &p.id == id));
        pick_list(self.projects.as_slice(), selected, Message::ProjectSelected)
            .placeholder("Choose a project")
            .text_size(SMALL)
            .padding([6.0, 10.0])
            .width(Length::Fixed(170.0))
            .style(style::pick)
            .into()
    }

    fn can_toggle(&self) -> bool {
        self.project.is_some()
    }

    /// Whether the card under header `key` shows. Every card shows while a
    /// filter is typed.
    fn block_open(&self, key: &str) -> bool {
        !self.filter.is_empty() || self.open_blocks.get(key).copied().unwrap_or(true)
    }

    /// Whether the list `key` of `len` rows shows all of them rather than
    /// its first [`PREVIEW`].
    fn show_all(&self, key: &str, len: usize) -> bool {
        !self.filter.is_empty()
            || len <= COLLAPSE_AT
            || self.open_blocks.get(&all_key(key)).copied().unwrap_or(false)
    }

    /// A header above a card: a chevron, the title and its count, which
    /// opens and closes the card, then `trailing` on the right.
    fn block_header<'a>(
        &self,
        key: &str,
        title: String,
        count: Option<usize>,
        trailing: Option<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        let open = self.block_open(key);
        let filtering = !self.filter.is_empty();
        let mut label = row![].spacing(6).align_y(Alignment::Center);
        if !filtering {
            label = label.push(muted(
                if open { Icon::ChevronDown } else { Icon::ChevronRight },
                ICON,
            ));
        }
        label = label.push(text(title).size(SMALL).wrapping(text::Wrapping::None));
        if let Some(count) = count {
            label = label.push(
                text(count.to_string())
                    .size(SMALL)
                    .style(style::faint),
            );
        }
        let toggle = button(label)
            .padding([3.0, 0.0])
            .style(style::header)
            .on_press_maybe((!filtering).then(|| Message::BlockOpen(key.to_owned(), !open)));
        let mut line = row![toggle, space().width(Fill)]
            .spacing(SPACING)
            .align_y(Alignment::Center);
        if let Some(trailing) = trailing {
            line = line.push(trailing);
        }
        line.into()
    }

    /// The tool rows of list `key`, cut to a preview when long, with a
    /// last row that shows all or fewer.
    fn tool_rows<'a>(&'a self, key: &str, tools: Vec<ToolRow<'a>>) -> Vec<Element<'a, Message>> {
        let total = tools.len();
        let all = self.show_all(key, total);
        let shown = if all { total } else { PREVIEW };
        let mut rows: Vec<_> = tools
            .into_iter()
            .take(shown)
            .map(|tool| self.tool_row(tool))
            .collect();
        if total > COLLAPSE_AT && self.filter.is_empty() {
            let label = if all {
                "Show fewer".to_owned()
            } else {
                format!("Show all {total}")
            };
            rows.push(more_row(label, Message::BlockOpen(all_key(key), !all)));
        }
        rows
    }

    fn builtin_view<'a>(&'a self, catalog: Catalog<'a>, wide: bool) -> Element<'a, Message> {
        if catalog.builtin.is_empty() {
            return note("No built in tool matches the filter").into();
        }
        let groups: Vec<_> = catalog
            .builtin
            .into_iter()
            .map(|group| self.group_view(group))
            .collect();
        grid(groups, wide)
    }

    fn group_view<'a>(&'a self, group: GroupView<'a>) -> Element<'a, Message> {
        let (key, title) = match group.group {
            BuiltinGroup::Harness if group.title.is_empty() => {
                ("harness", "Harness tools".to_owned())
            }
            BuiltinGroup::Harness => ("harness", format!("Harness tools · {}", group.title)),
            BuiltinGroup::App => ("app", group.title.to_owned()),
        };
        let header = self.block_header(key, title, Some(group.tools.len()), None);
        if !self.block_open(key) {
            return header;
        }
        let harness_note = (group.group == BuiltinGroup::Harness)
            .then_some(self.builtin.harness_note.as_deref())
            .flatten();
        let rows = if let Some(text) = harness_note {
            vec![note_row(text)]
        } else if group.tools.is_empty() {
            vec![note_row("No tools")]
        } else {
            self.tool_rows(key, group.tools)
        };
        column![header, card(rows)].spacing(6).into()
    }

    /// One tool: checkbox, name, clipped description and detail dots on a
    /// line; a click opens its full description and details under it.
    fn tool_row<'a>(&'a self, tool: ToolRow<'a>) -> Element<'a, Message> {
        let ToolRow { id, info, enabled } = tool;
        let expanded = self.expanded_tool.as_ref() == Some(&id);
        let on_toggle = self.can_toggle().then(|| {
            let id = id.clone();
            move |on| Message::ToolToggled(id.clone(), on)
        });
        let mut line = row![
            checkbox(enabled)
                .size(14)
                .style(style::check)
                .on_toggle_maybe(on_toggle),
            text(info.name.as_str())
                .size(BODY)
                .wrapping(text::Wrapping::None),
            clipped(
                text(info.description.as_str())
                    .size(SMALL)
                    .style(style::muted)
                    .wrapping(text::Wrapping::None)
            ),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        if !info.details.is_empty() {
            line = line.push(
                row(info
                    .details
                    .iter()
                    .map(|detail| status_dot(detail_tone(detail.state))))
                .spacing(4)
                .align_y(Alignment::Center),
            );
        }
        line = line.push(muted(
            if expanded { Icon::ChevronUp } else { Icon::ChevronDown },
            ICON,
        ));
        let head = button(line)
            .padding(ROW_PAD)
            .width(Fill)
            .style(style::row)
            .on_press(Message::ToolExpanded(id));
        if !expanded {
            return head.into();
        }
        column![head, tool_details(info)].into()
    }

    fn servers_view<'a>(&'a self, catalog: Catalog<'a>, wide: bool) -> Element<'a, Message> {
        let actions = row![
            flat(with_icon(Icon::Plus, "Add server")).on_press(Message::AddServerOpen),
            flat(text("Import").size(SMALL)).on_press(Message::ImportScan),
        ]
        .spacing(2);
        let header = row![
            text("Servers").size(SMALL).style(style::muted),
            text(catalog.servers.len().to_string())
                .size(SMALL)
                .style(style::faint),
            space().width(Fill),
            actions,
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let mut content = column![header].spacing(10);
        if let Some(form) = self.add_server_form() {
            content = content.push(form);
        }
        if let Some(imports) = self.import_view() {
            content = content.push(imports);
        }
        if catalog.servers.is_empty() {
            let note_text = if self.filter.is_empty() {
                "No MCP servers"
            } else {
                "No server matches the filter"
            };
            content = content.push(card(vec![note_row(note_text)]));
        }
        let servers: Vec<_> = catalog
            .servers
            .into_iter()
            .map(|server| self.server_view(server))
            .collect();
        content.push(grid(servers, wide)).into()
    }

    /// A server's card: its name and status (which open and close its
    /// tools), its transport and controls, then its tools.
    fn server_view<'a>(&'a self, view: ServerView<'a>) -> Element<'a, Message> {
        let server = view.server;
        let name = server.name.as_str();
        let key = format!("server:{name}");
        let open = self.block_open(&key);
        let (tone, status_label) = match &server.status {
            ServerStatus::Running => (Tone::Ok, "running".to_owned()),
            ServerStatus::Starting => (Tone::Off, "starting".to_owned()),
            ServerStatus::Stopped => (Tone::Off, "stopped".to_owned()),
            ServerStatus::Error(message) => (Tone::Bad, format!("error: {message}")),
        };
        let kind = match server.kind {
            ServerKind::Surface => "surface",
            ServerKind::AppHosted => "app",
            ServerKind::External => "",
        };
        let filtering = !self.filter.is_empty();
        let mut title = row![
            status_dot(tone),
            text(name)
                .size(BODY)
                .font(BOLD)
                .wrapping(text::Wrapping::None),
            text(kind).size(SMALL).style(style::faint),
            clipped(
                text(status_label)
                    .size(SMALL)
                    .style(style::tone_text(tone))
                    .wrapping(text::Wrapping::None)
                    .width(Fill)
                    .align_x(Alignment::End)
            ),
            text(server.tools.len().to_string())
                .size(SMALL)
                .style(style::faint),
        ]
        .spacing(SPACING)
        .align_y(Alignment::Center);
        if !filtering {
            title = title.push(muted(
                if open { Icon::ChevronUp } else { Icon::ChevronDown },
                ICON,
            ));
        }
        let title = button(title)
            .padding(ROW_PAD)
            .width(Fill)
            .style(style::row)
            .on_press_maybe((!filtering).then(|| Message::BlockOpen(key.clone(), !open)));

        let action = |label: &'a str, action: ServerAction, enabled: bool| {
            flat(text(label).size(SMALL))
                .on_press_maybe(enabled.then(|| Message::Server(name.to_owned(), action)))
        };
        let running = matches!(server.status, ServerStatus::Running | ServerStatus::Starting);
        let toggle = flat(text(format!("Tools: {}", view.toggle.label())).size(SMALL))
            .on_press_maybe(
                self.can_toggle()
                    .then(|| Message::ServerToggled(name.to_owned())),
            );
        let mut actions = row![toggle].spacing(2);
        // App hosted servers run with the app; only their tools toggle.
        if server.kind != ServerKind::AppHosted {
            actions = actions
                .push(action("Start", ServerAction::Start, !running))
                .push(action("Stop", ServerAction::Stop, running))
                .push(action("Restart", ServerAction::Restart, running));
        }
        if server.can_remove() {
            actions = actions.push(
                flat(text("Remove").size(SMALL))
                    .style(style::danger_flat)
                    .on_press(Message::Server(name.to_owned(), ServerAction::Remove)),
            );
        }
        let controls = container(
            column![
                text(transport_label(&server.transport))
                    .size(SMALL)
                    .style(style::faint)
                    .font(Font::MONOSPACE)
                    .wrapping(text::Wrapping::WordOrGlyph),
                actions,
            ]
            .spacing(2),
        )
        .padding(Padding {
            top: 0.0,
            right: 6.0,
            bottom: 6.0,
            left: 10.0,
        })
        .width(Fill);

        let mut rows: Vec<Element<'a, Message>> =
            vec![column![title, controls].into()];
        if open {
            if view.tools.is_empty() {
                rows.push(note_row("No tools declared"));
            } else {
                rows.extend(self.tool_rows(&key, view.tools));
            }
        }
        card(rows)
    }

    fn add_server_form(&self) -> Option<Element<'_, Message>> {
        let (form, errors) = self.add_server.as_ref()?;
        let kind = |label: &'static str, kind: TransportKind| {
            button(text(label).size(SMALL))
                .padding([4.0, 10.0])
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
            row![
                text("Add server").size(BODY).font(BOLD),
                space().width(Fill),
                row![
                    kind("stdio", TransportKind::Stdio),
                    kind("HTTP", TransportKind::Http)
                ]
                .spacing(2),
            ]
            .align_y(Alignment::Center),
            field("Name", &form.name, Message::AddServerName).on_submit(Message::AddServerSubmit),
            field_error(errors.name.as_ref()),
            target,
            field_error(errors.target.as_ref()),
            form_buttons("Add", Message::AddServerSubmit, Message::AddServerCancel),
        ]
        .spacing(6);
        Some(form_card(content))
    }

    fn import_view(&self) -> Option<Element<'_, Message>> {
        let entries = self.imports.as_ref()?;
        let header = row![
            text("Import from .claude, .cursor, .codex").size(BODY).font(BOLD),
            space().width(Fill),
            flat(with_icon(Icon::X, "Close")).on_press(Message::ImportClose),
        ]
        .align_y(Alignment::Center);
        let mut rows: Vec<Element<'_, Message>> = vec![container(header)
            .padding(Padding { top: 4.0, right: 2.0, bottom: 4.0, left: 10.0 })
            .into()];
        if self.import_scanning {
            rows.push(note_row("Scanning..."));
        } else if entries.is_empty() {
            rows.push(note_row("No server configs found"));
        }
        for (index, entry) in entries.iter().enumerate() {
            let candidate = &entry.candidate;
            let status: Element<'_, Message> = match &entry.outcome {
                None if entry.exists => text("already registered")
                    .size(SMALL)
                    .style(style::warn_text)
                    .into(),
                None => space().width(0).into(),
                Some(crate::ImportOutcome::Imported) => {
                    text("imported").size(SMALL).style(text::success).into()
                }
                Some(crate::ImportOutcome::Skipped(why)) => text(format!("skipped: {why}"))
                    .size(SMALL)
                    .style(style::muted)
                    .into(),
                Some(crate::ImportOutcome::Failed(why)) => text(format!("failed: {why}"))
                    .size(SMALL)
                    .style(text::danger)
                    .into(),
            };
            let line = row![
                checkbox(entry.selected)
                    .size(14)
                    .style(style::check)
                    .on_toggle(move |on| Message::ImportSelect(index, on)),
                text(candidate.server.name.as_str())
                    .size(BODY)
                    .wrapping(text::Wrapping::None),
                clipped(
                    text(format!("{} {}", candidate.source.label(), candidate.path))
                        .size(SMALL)
                        .style(style::faint)
                        .wrapping(text::Wrapping::None)
                ),
                status,
            ]
            .spacing(10)
            .align_y(Alignment::Center);
            rows.push(container(line).padding(ROW_PAD).width(Fill).into());
        }
        let any_selected = entries.iter().any(|e| e.selected);
        rows.push(
            container(
                filled("Import selected")
                    .on_press_maybe(any_selected.then_some(Message::ImportSubmit)),
            )
            .padding(ROW_PAD)
            .into(),
        );
        Some(card(rows))
    }

    /// The hosts that match the filter.
    fn visible_hosts(&self) -> Vec<&RemoteHost> {
        self.hosts
            .iter()
            .filter(|h| self.filter.matches(&h.alias) || self.filter.matches(&h.hostname))
            .collect()
    }

    fn hosts_view(&self, wide: bool) -> Element<'_, Message> {
        let mut content = column![].spacing(10);
        if !self.prompts.is_empty() {
            let mut prompts = column![
                row![
                    text("Waiting for you").size(SMALL).style(style::warn_text),
                    text(self.prompts.len().to_string())
                        .size(SMALL)
                        .style(style::faint),
                ]
                .spacing(6)
            ]
            .spacing(6);
            for prompt in &self.prompts {
                prompts = prompts.push(prompt_card(prompt));
            }
            content = content.push(prompts);
        }
        if let Some((form, errors)) = &self.add_host {
            let form = column![
                text("Add host").size(BODY).font(BOLD),
                pair(
                    column![
                        field("Alias", &form.alias, Message::AddHostAlias)
                            .on_submit(Message::AddHostSubmit),
                        field_error(errors.alias.as_ref())
                    ],
                    column![
                        field("Hostname", &form.hostname, Message::AddHostHostname)
                            .on_submit(Message::AddHostSubmit),
                        field_error(errors.hostname.as_ref())
                    ],
                ),
                pair(
                    column![
                        field("User (optional)", &form.user, Message::AddHostUser)
                            .on_submit(Message::AddHostSubmit),
                        field_error(errors.user.as_ref())
                    ],
                    column![
                        field("Port (optional)", &form.port, Message::AddHostPort)
                            .on_submit(Message::AddHostSubmit),
                        field_error(errors.port.as_ref())
                    ],
                ),
                form_buttons("Add", Message::AddHostSubmit, Message::AddHostCancel),
            ]
            .spacing(6);
            content = content.push(form_card(form));
        }

        let hosts = self.visible_hosts();
        let actions = row![
            flat(text("Import from ssh config").size(SMALL)).on_press(Message::ImportSshHosts),
            flat(with_icon(Icon::Plus, "Add host")).on_press(Message::AddHostOpen),
        ]
        .spacing(2);
        let header = self.block_header("hosts", "Hosts".to_owned(), Some(hosts.len()), Some(actions.into()));
        content = content.push(header);
        if self.block_open("hosts") {
            let rows = if hosts.is_empty() {
                vec![note_row("No remote hosts")]
            } else {
                hosts
                    .into_iter()
                    .map(|host| self.host_view(host, wide))
                    .collect()
            };
            content = content.push(card(rows));
        }
        content.into()
    }

    /// A host's row: alias and address, its limits in a line, its actions;
    /// the limits editor and audit log open inline under it.
    fn host_view<'a>(&'a self, host: &'a RemoteHost, wide: bool) -> Element<'a, Message> {
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
            "{} · {} · {} · {}",
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
            if limits.auto_approve {
                "runs without asking"
            } else {
                "asks first"
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
        .spacing(2);
        let name = row![
            text(alias)
                .size(BODY)
                .font(BOLD)
                .wrapping(text::Wrapping::None),
            clipped(
                text(target)
                    .size(SMALL)
                    .style(style::muted)
                    .font(Font::MONOSPACE)
                    .wrapping(text::Wrapping::None)
            ),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let summary = clipped(
            text(summary)
                .size(SMALL)
                .style(style::faint)
                .wrapping(text::Wrapping::None),
        );
        let head: Element<'a, Message> = if wide {
            column![
                row![name, actions].spacing(SPACING).align_y(Alignment::Center),
                summary
            ]
            .spacing(2)
            .into()
        } else {
            column![name, summary, actions].spacing(2).into()
        };
        let mut content = column![container(head).padding(Padding {
            top: 6.0,
            right: 4.0,
            bottom: 6.0,
            left: 10.0
        })];
        if let Some(draft) = self.limits.as_ref().filter(|d| d.host == host.alias) {
            let mut paths = column![].spacing(2);
            for (index, path) in draft.limits.allowed_paths.iter().enumerate() {
                paths = paths.push(
                    row![
                        text(path.as_str())
                            .size(SMALL)
                            .font(Font::MONOSPACE)
                            .width(Fill),
                        flat(text("Remove").size(SMALL))
                            .style(style::danger_flat)
                            .on_press(Message::LimitsRemovePath(index)),
                    ]
                    .align_y(Alignment::Center),
                );
            }
            let check = |on: bool, label: &'static str, message: fn(bool) -> Message| {
                checkbox(on)
                    .label(label)
                    .size(14)
                    .text_size(SMALL)
                    .spacing(SPACING)
                    .style(style::check)
                    .on_toggle(message)
            };
            let editor = column![
                row![
                    check(draft.limits.read_only, "Read only", Message::LimitsReadOnly),
                    check(draft.limits.commands_allowed, "Commands allowed", Message::LimitsCommands),
                ]
                .spacing(GAP),
                check(
                    draft.limits.auto_approve,
                    "Run writes and commands without asking",
                    Message::LimitsAutoApprove
                ),
                text("Allowed paths").size(SMALL).style(style::muted),
                paths,
                row![
                    field("/path or ~/path", &draft.new_path, Message::LimitsNewPath)
                        .on_submit(Message::LimitsAddPath),
                    flat(with_icon(Icon::Plus, "Add path")).on_press(Message::LimitsAddPath),
                ]
                .spacing(4)
                .align_y(Alignment::Center),
                field_error(draft.path_error.as_ref()),
                form_buttons("Save", Message::LimitsSave, Message::LimitsCancel),
            ]
            .spacing(8);
            content = content.push(inset(editor));
        }
        if let Some((_, rows)) = self.audit.as_ref().filter(|(h, _)| h == alias) {
            content = content.push(inset(audit_log(rows)));
        }
        content.into()
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
        let header = row![
            text("Tools each peer's grant allows")
                .size(SMALL)
                .style(style::muted)
                .width(Fill),
            pick_list(choices, selected, Message::PeerFilter)
                .text_size(SMALL)
                .padding([5.0, 10.0])
                .style(style::pick),
        ]
        .spacing(SPACING)
        .align_y(Alignment::Center);
        let content = column![header].spacing(10);

        let matrix = self.peer_matrix();
        if matrix.peers.is_empty() {
            return content.push(card(vec![note_row("No peers")])).into();
        }
        let mut head = row![text("Tool").size(SMALL).style(style::muted).width(Fill)]
            .spacing(SPACING);
        for peer in &matrix.peers {
            head = head.push(
                text(peer.name.as_str())
                    .size(SMALL)
                    .style(style::muted)
                    .wrapping(text::Wrapping::None)
                    .width(PEER_COLUMN),
            );
        }
        let mut rows: Vec<Element<'_, Message>> =
            vec![container(head).padding(ROW_PAD).width(Fill).into()];
        if matrix.rows.is_empty() {
            rows.push(note_row("No tools"));
        }
        for line in matrix.rows {
            let label = match &line.tool {
                ToolId::Builtin(name) => name.clone(),
                ToolId::Mcp { server, tool } => format!("{server} / {tool}"),
            };
            let mut cells = row![clipped(
                text(label).size(SMALL).wrapping(text::Wrapping::None)
            )]
            .spacing(SPACING);
            for allowed in line.allowed {
                cells = cells.push(if allowed {
                    text("allowed").size(SMALL).width(PEER_COLUMN).style(text::success)
                } else {
                    text("no").size(SMALL).width(PEER_COLUMN).style(style::faint)
                });
            }
            rows.push(
                container(cells)
                    .padding(Padding { top: 5.0, ..ROW_PAD }.bottom(5.0))
                    .width(Fill)
                    .into(),
            );
        }
        content.push(card(rows)).into()
    }
}

/// Key under which list `key` remembers it shows all its rows.
fn all_key(key: &str) -> String {
    format!("{key}:all")
}

/// A panel tab: label and faint count over a 2px underline when active.
fn tab<'a>(label: String, count: usize, section: Section, current: Section) -> Element<'a, Message> {
    let active = section == current;
    let waiting = section == Section::Hosts && label.ends_with(')');
    let label = text(label)
        .size(13)
        .wrapping(text::Wrapping::None)
        .style(if active {
            text::base
        } else if waiting {
            style::warn_text
        } else {
            style::muted
        });
    let mut line = row![label].spacing(5).align_y(Alignment::Center);
    if count > 0 {
        line = line.push(
            text(count.to_string())
                .size(SMALL)
                .style(style::faint)
                .wrapping(text::Wrapping::None),
        );
    }
    button(
        column![
            line,
            container(space().height(2))
                .width(Fill)
                .style(style::underline(active)),
        ]
        .width(Shrink)
        .spacing(6)
        .align_x(Alignment::Center),
    )
    .padding([4.0, 0.0])
    .style(style::plain)
    .on_press(Message::SectionSelected(section))
    .into()
}

/// An open tool: its whole description and its details, under its name.
fn tool_details(info: &ToolInfo) -> Element<'_, Message> {
    let mut content = column![
        text(info.description.as_str())
            .size(SMALL)
            .style(style::muted)
    ]
    .spacing(4);
    for detail in &info.details {
        content = content.push(
            row![
                status_dot(detail_tone(detail.state)),
                text(detail.label.as_str()).size(SMALL).width(DETAIL_LABEL),
                text(detail.text.as_str()).size(SMALL).style(style::muted),
            ]
            .spacing(SPACING)
            .align_y(Alignment::Center),
        );
    }
    container(content)
        .padding(Padding {
            top: 0.0,
            right: 10.0,
            bottom: 10.0,
            left: DETAIL_INSET,
        })
        .width(Fill)
        .into()
}

fn detail_tone(state: DetailState) -> Tone {
    match state {
        DetailState::Ready => Tone::Ok,
        DetailState::NotConfigured => Tone::Off,
        DetailState::Unavailable => Tone::Bad,
    }
}

/// A host's audit log, newest first, in a scroll area of its own.
fn audit_log(rows: &[AuditRow]) -> Element<'_, Message> {
    let header = row![
        text("Audit log").size(SMALL).style(style::muted),
        text(rows.len().to_string()).size(SMALL).style(style::faint),
        space().width(Fill),
        flat(with_icon(Icon::X, "Close")).on_press(Message::CloseAudit),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if rows.is_empty() {
        return column![header, text("No entries").size(SMALL).style(style::faint)]
            .spacing(4)
            .into();
    }
    let mut log = column![].spacing(6);
    for (index, entry) in rows.iter().enumerate() {
        if index > 0 {
            log = log.push(style::divider());
        }
        let ok = entry.outcome == "ok" || entry.outcome.starts_with("ok:");
        log = log.push(
            column![
                row![
                    text(entry.action.label())
                        .size(SMALL)
                        .wrapping(text::Wrapping::None),
                    clipped(
                        text(entry.detail.as_str())
                            .size(SMALL)
                            .font(Font::MONOSPACE)
                            .wrapping(text::Wrapping::None)
                    ),
                    text(entry.when.as_str())
                        .size(SMALL)
                        .style(style::faint)
                        .wrapping(text::Wrapping::None),
                ]
                .spacing(SPACING),
                row![
                    clipped(
                        text(entry.outcome.as_str())
                            .size(SMALL)
                            .wrapping(text::Wrapping::None)
                            .style(if ok { text::success } else { text::danger })
                    ),
                    text(entry.session.as_str())
                        .size(SMALL)
                        .style(style::faint)
                        .wrapping(text::Wrapping::None),
                ]
                .spacing(SPACING),
            ]
            .spacing(2),
        );
    }
    let log = scrollable(container(log).padding(Padding::ZERO.right(10)))
        .direction(style::thin_scrollbar())
        .style(style::scrollbar)
        .height(Length::Shrink);
    column![header, container(log).max_height(AUDIT_HEIGHT)]
        .spacing(4)
        .into()
}

/// A host key or approval question with its answers.
fn prompt_card(prompt: &RemotePrompt) -> Element<'_, Message> {
    let answer = |label: &'static str, answer: PromptAnswer| {
        flat(text(label).size(SMALL)).on_press(Message::AnswerPrompt(prompt.id, answer))
    };
    let (accept, reject) = match prompt.kind {
        PromptKind::HostKey => (
            row![answer("Trust", PromptAnswer::Trust)],
            answer("Reject", PromptAnswer::Reject),
        ),
        PromptKind::Approval => (
            row![
                answer("Allow once", PromptAnswer::AllowOnce),
                answer("Allow for session", PromptAnswer::AllowSession),
            ]
            .spacing(2),
            answer("Deny", PromptAnswer::Deny),
        ),
    };
    let content = column![
        row![
            icon(Icon::TriangleAlert, ICON).style(|theme: &iced::Theme, _| {
                iced::widget::svg::Style {
                    color: Some(theme.extended_palette().warning.base.color),
                }
            }),
            text(prompt.title.as_str()).size(BODY),
        ]
        .spacing(SPACING)
        .align_y(Alignment::Center),
        text(prompt.body.as_str())
            .size(SMALL)
            .font(Font::MONOSPACE)
            .style(style::muted),
        row![accept, reject.style(style::danger_flat)].spacing(2),
    ]
    .spacing(6);
    container(content)
        .padding(FORM_PAD)
        .width(Fill)
        .style(style::attention_card)
        .into()
}

/// Rows in a bordered card with a divider between each two.
fn card<'a>(rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    let mut content = column![];
    for (index, item) in rows.into_iter().enumerate() {
        if index > 0 {
            content = content.push(container(style::divider()).padding([0.0, 6.0]));
        }
        content = content.push(item);
    }
    container(content)
        .padding(CARD_PAD)
        .width(Fill)
        .style(style::card)
        .into()
}

/// A form or editor in a bordered card.
fn form_card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(FORM_PAD)
        .width(Fill)
        .style(style::card)
        .into()
}

/// An open editor or log under a row, set off by a faint fill.
fn inset<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(
        container(content)
            .padding(FORM_PAD)
            .width(Fill)
            .style(style::inset),
    )
    .padding(Padding::ZERO.bottom(4))
    .into()
}

/// Cards in two columns when `wide`, else stacked.
fn grid<'a>(items: Vec<Element<'a, Message>>, wide: bool) -> Element<'a, Message> {
    if !wide {
        return column(items).spacing(GAP).into();
    }
    let mut rows = column![].spacing(GAP);
    let mut items = items.into_iter();
    while let Some(first) = items.next() {
        let second = items
            .next()
            .unwrap_or_else(|| space().width(Fill).into());
        rows = rows.push(
            row![
                container(first).width(Fill),
                container(second).width(Fill)
            ]
            .spacing(GAP)
            .align_y(Alignment::Start),
        );
    }
    rows.into()
}

/// Two form fields side by side.
fn pair<'a>(
    left: impl Into<Element<'a, Message>>,
    right: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![
        container(left).width(Fill),
        container(right).width(Fill)
    ]
    .spacing(SPACING)
    .into()
}

/// A muted line inside a card, for empty states and progress.
fn note_row(label: &str) -> Element<'_, Message> {
    container(note(label)).padding(ROW_PAD).width(Fill).into()
}

/// The last row of a cut list: shows all of it, or fewer.
fn more_row<'a>(label: String, message: Message) -> Element<'a, Message> {
    button(text(label).size(SMALL).style(style::muted))
        .padding(ROW_PAD)
        .width(Fill)
        .style(style::row)
        .on_press(message)
        .into()
}

/// Content that takes the rest of a line and is cut at its edge.
fn clipped<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content).width(Fill).clip(true).into()
}

/// A 6px status dot.
fn status_dot<'a>(tone: Tone) -> Element<'a, Message> {
    container(space().width(6).height(6))
        .style(style::dot(tone))
        .into()
}

/// Add and Cancel of a form.
fn form_buttons<'a>(submit: &'a str, on_submit: Message, on_cancel: Message) -> Element<'a, Message> {
    row![
        filled(submit).on_press(on_submit),
        flat(text("Cancel").size(SMALL)).on_press(on_cancel),
    ]
    .spacing(SPACING)
    .into()
}

/// A small flat button: secondary text, a fill only when hovered.
fn flat<'a>(content: impl Into<Element<'a, Message>>) -> button::Button<'a, Message> {
    button(content).padding([4.0, 8.0]).style(style::flat)
}

/// The one filled action of a form: small and neutral.
fn filled(label: &str) -> button::Button<'_, Message> {
    button(text(label).size(SMALL))
        .padding([4.0, 12.0])
        .style(button::primary)
}

/// A flat button with a leading icon.
fn with_icon(glyph: Icon, label: &str) -> Element<'_, Message> {
    row![icon(glyph, ICON), text(label).size(SMALL)]
        .spacing(6)
        .align_y(Alignment::Center)
        .into()
}

/// A bordered text field, styled like every other input in the panel.
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

/// A bordered search box with a magnifier icon and a borderless field.
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
        .align_y(Alignment::Center),
    )
    .padding([6.0, 10.0])
    .width(Fill)
    .style(style::search_box)
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
