//! The left sidebar.
//!
//! From the top: a machine switcher that says which machine you are working
//! on and lists your peers, New session, Search, Projects, Pinned and
//! Recents for the chosen machine, and a footer with Usage, Configs and
//! Settings. Sessions with an open decision say so in the needs-you color.

use std::collections::BTreeSet;

use crate::data::{NodeFilter, NodeStatus, Nodes, SessionSummary, UsageMeter};
use iced::widget::{Column, button, column, container, hover, row, rule, scrollable, space, text, text_input};
use iced::{Alignment, Element, Fill, Padding, Task};
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{ColorRole as C, RadiusRole, SpaceRole as Sp, TextRole as T, pad_x, sp, text_size as ts, tokens};
use loa_ui_kit::widgets::{label, meter};

/// Id of the search input, for focusing it from a shortcut.
pub const SEARCH_ID: &str = "loa-sidebar-search";

/// Container id of the machine switcher.
pub const MACHINE_SWITCHER_ID: &str = "loa-sidebar-machine";

/// Container id of the waiting label on a session row.
pub fn waiting_badge_id(session: &str) -> String {
    format!("loa-sidebar-waiting-{session}")
}

/// Sidebar state: the sessions, projects and nodes the app passed in, the
/// search query, the chosen machine and the highlighted session.
#[derive(Debug, Clone, Default)]
pub struct State {
    sessions: Vec<Entry>,
    projects: Vec<String>,
    nodes: Nodes,
    /// Ids of the pinned sessions. App data, so it outlives every summary
    /// the app passes in, including sessions not listed yet.
    pins: BTreeSet<String>,
    /// Names of the pinned projects; names not in the list stay saved but hidden.
    pinned_projects: BTreeSet<String>,
    query: String,
    active: Option<String>,
    /// The project the user selected; highlighted instead of the active
    /// session's project while set.
    active_project: Option<String>,
    /// The project whose page fills the center, if one does.
    shown_project: Option<String>,
    /// Updates waiting for the user; the Updates row shows while non-zero.
    updates: usize,
    /// The machine the user picked; `None` means this computer.
    machine: Option<NodeFilter>,
    /// Whether the machine list is open.
    machines_open: bool,
    /// The footer usage meter, once usage is known.
    usage: Option<UsageMeter>,
}

/// A session with its lowercase search text, rebuilt when sessions or node
/// names change so typing in the search box does not allocate per row.
#[derive(Debug, Clone)]
struct Entry {
    summary: SessionSummary,
    haystack: String,
}

impl Entry {
    fn new(summary: SessionSummary, nodes: &Nodes) -> Self {
        let mut haystack = String::with_capacity(summary.title.len() + 32);
        haystack.push_str(&summary.title.to_lowercase());
        if let Some(project) = &summary.project {
            haystack.push('\n');
            haystack.push_str(&project.to_lowercase());
        }
        haystack.push('\n');
        haystack.push_str(&nodes.name_of(&summary.node).to_lowercase());
        Self { summary, haystack }
    }
}

/// Internal messages.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// The New session button.
    New,
    /// The search text changed.
    Search(String),
    /// A project row was clicked.
    OpenProject(String),
    /// Manage peers, from the machine list.
    OpenPeers,
    /// The Tools item was clicked.
    OpenTools,
    /// The Updates item was clicked.
    OpenUpdates,
    /// The Usage item was clicked.
    OpenUsage,
    /// The Configs item was clicked.
    OpenConfigs,
    /// The Settings item was clicked.
    OpenSettings,
    /// The machine switcher was clicked.
    ToggleMachines,
    /// A machine was picked in the switcher.
    PickMachine(NodeFilter),
    /// A session row was clicked.
    OpenSession(String),
    /// The pin button on a session row was clicked.
    TogglePin(String),
    /// The pin button on a project row was clicked.
    ToggleProjectPin(String),
    /// The delete button on a session row was clicked.
    Delete(String),
}

/// What the app must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Start a new session.
    NewSession,
    /// Show this session.
    OpenSession(String),
    /// Show this project.
    OpenProject(String),
    /// Show the peers list.
    OpenPeers,
    /// Show the tools panel.
    OpenTools,
    /// Show the updates screen.
    OpenUpdates,
    /// Show the usage window.
    OpenUsage,
    /// Show the configs screen.
    OpenConfigs,
    /// Show the settings window.
    OpenSettings,
    /// Persist the pinned flag of a session.
    SetPinned {
        /// Session id.
        id: String,
        /// New flag.
        pinned: bool,
    },
    /// Persist the pinned flag of a project.
    SetProjectPinned {
        /// Project name.
        project: String,
        /// New flag.
        pinned: bool,
    },
    /// Delete this session and its journal.
    DeleteSession(String),
}

impl State {
    /// Replaces every session.
    pub fn set_sessions(&mut self, sessions: impl IntoIterator<Item = SessionSummary>) {
        let entries = sessions.into_iter().map(|summary| self.entry(summary)).collect();
        self.sessions = entries;
    }

    /// Adds a session or replaces the one with the same id. Its pinned flag
    /// comes from the pins; a summary that says pinned adds a pin.
    pub fn upsert_session(&mut self, summary: SessionSummary) {
        let entry = self.entry(summary);
        match self.sessions.iter_mut().find(|existing| existing.summary.id == entry.summary.id) {
            Some(existing) => *existing = entry,
            None => self.sessions.push(entry),
        }
    }

    fn entry(&mut self, mut summary: SessionSummary) -> Entry {
        if summary.pinned {
            self.pins.insert(summary.id.clone());
        } else {
            summary.pinned = self.pins.contains(&summary.id);
        }
        Entry::new(summary, &self.nodes)
    }

    /// Replaces the pinned session ids, as saved by the app.
    pub fn set_pins(&mut self, pins: impl IntoIterator<Item = String>) {
        self.pins = pins.into_iter().collect();
        for entry in &mut self.sessions {
            entry.summary.pinned = self.pins.contains(&entry.summary.id);
        }
    }

    /// Ids of the pinned sessions, listed or not.
    pub fn pins(&self) -> &BTreeSet<String> {
        &self.pins
    }

    /// Drops a session.
    pub fn remove_session(&mut self, id: &str) {
        self.sessions.retain(|entry| entry.summary.id != id);
    }

    /// The session with this id.
    pub fn session(&self, id: &str) -> Option<&SessionSummary> {
        self.sessions.iter().map(|entry| &entry.summary).find(|summary| summary.id == id)
    }

    /// Replaces the pinned project names, as saved by the app.
    pub fn set_pinned_projects(&mut self, pinned: impl IntoIterator<Item = String>) {
        self.pinned_projects = pinned.into_iter().collect();
    }

    /// The pinned project names, including any no longer listed.
    pub fn pinned_projects(&self) -> &BTreeSet<String> {
        &self.pinned_projects
    }

    /// Highlights a project row. While set it replaces the active session's
    /// project as the highlighted one.
    pub fn set_active_project(&mut self, project: Option<String>) {
        self.active_project = project;
    }

    /// Replaces the project list.
    pub fn set_projects(&mut self, projects: impl IntoIterator<Item = String>) {
        self.projects = projects.into_iter().collect();
    }

    /// Replaces the node directory and refreshes node tags.
    pub fn set_nodes(&mut self, nodes: Nodes) {
        self.nodes = nodes;
        let sessions = std::mem::take(&mut self.sessions);
        self.sessions = sessions.into_iter().map(|entry| Entry::new(entry.summary, &self.nodes)).collect();
    }

    /// The node directory.
    pub fn nodes(&self) -> &Nodes {
        &self.nodes
    }

    /// Highlights a session row.
    pub fn set_active(&mut self, id: Option<String>) {
        self.active = id;
    }

    /// Marks the project whose page is open; it is highlighted instead of
    /// the active session.
    pub fn set_shown_project(&mut self, project: Option<String>) {
        self.shown_project = project;
    }

    /// The highlighted session.
    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// Sets how many updates wait for the user; the sidebar lists an
    /// Updates row while it is non-zero.
    pub fn set_updates(&mut self, count: usize) {
        self.updates = count;
    }

    /// How many updates wait for the user.
    pub fn updates(&self) -> usize {
        self.updates
    }

    /// Sets the footer usage meter.
    pub fn set_usage(&mut self, usage: Option<UsageMeter>) {
        self.usage = usage;
    }

    /// The search text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The machine whose sessions are listed: the user's pick, else this
    /// computer, else every machine when no local node is known yet.
    pub fn machine(&self) -> NodeFilter {
        if let Some(machine) = &self.machine {
            return machine.clone();
        }
        match self.nodes.iter().find(|node| node.local) {
            Some(local) => NodeFilter::Node(local.key.clone()),
            None => NodeFilter::All,
        }
    }

    /// Whether the machine list is open.
    pub fn machines_open(&self) -> bool {
        self.machines_open
    }

    fn on_machine(&self, summary: &SessionSummary) -> bool {
        match self.machine() {
            NodeFilter::All => true,
            NodeFilter::Node(key) => summary.node == key,
        }
    }

    /// Pinned sessions on the chosen machine that match the search, most
    /// recently updated first.
    pub fn pinned(&self) -> Vec<&SessionSummary> {
        self.section(true)
    }

    /// Unpinned sessions on the chosen machine that match the search, most
    /// recently updated first.
    pub fn recents(&self) -> Vec<&SessionSummary> {
        self.section(false)
    }

    /// Projects whose name matches the search, in the order given.
    pub fn projects(&self) -> Vec<&str> {
        let query = self.query.to_lowercase();
        let mut pinned: Vec<&String> = self.projects.iter().filter(|project| self.pinned_projects.contains(*project)).collect();
        pinned.sort();
        pinned
            .into_iter()
            .chain(self.projects.iter().filter(|project| !self.pinned_projects.contains(*project)))
            .filter(|project| {
                let name = project.to_lowercase();
                query.split_whitespace().all(|term| name.contains(term))
            })
            .map(String::as_str)
            .collect()
    }

    fn section(&self, pinned: bool) -> Vec<&SessionSummary> {
        let query = self.query.to_lowercase();
        let mut list: Vec<&SessionSummary> = self
            .sessions
            .iter()
            .filter(|entry| entry.summary.pinned == pinned && self.on_machine(&entry.summary))
            .filter(|entry| query.split_whitespace().all(|term| entry.haystack.contains(term)))
            .map(|entry| &entry.summary)
            .collect();
        list.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
        list
    }

    /// Sessions and waiting sessions on a machine, for the switcher.
    fn counts(&self, filter: &NodeFilter) -> (usize, usize) {
        let on = |summary: &SessionSummary| match filter {
            NodeFilter::All => true,
            NodeFilter::Node(key) => &summary.node == key,
        };
        let list: Vec<&SessionSummary> = self.sessions.iter().map(|entry| &entry.summary).filter(|s| on(s)).collect();
        (list.len(), list.iter().filter(|s| s.waiting).count())
    }

    /// Handles a message.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::New => Some(Event::NewSession),
            Message::Search(query) => {
                self.query = query;
                None
            }
            Message::OpenProject(project) => Some(Event::OpenProject(project)),
            Message::OpenPeers => {
                self.machines_open = false;
                Some(Event::OpenPeers)
            }
            Message::OpenTools => Some(Event::OpenTools),
            Message::OpenUpdates => Some(Event::OpenUpdates),
            Message::OpenUsage => Some(Event::OpenUsage),
            Message::OpenConfigs => Some(Event::OpenConfigs),
            Message::OpenSettings => Some(Event::OpenSettings),
            Message::ToggleMachines => {
                self.machines_open = !self.machines_open;
                None
            }
            Message::PickMachine(filter) => {
                self.machine = Some(filter);
                self.machines_open = false;
                None
            }
            Message::ToggleProjectPin(project) => {
                let pinned = !self.pinned_projects.remove(&project);
                if pinned {
                    self.pinned_projects.insert(project.clone());
                }
                Some(Event::SetProjectPinned { project, pinned })
            }
            Message::OpenSession(id) => {
                self.active = Some(id.clone());
                Some(Event::OpenSession(id))
            }
            Message::TogglePin(id) => {
                let entry = self.sessions.iter_mut().find(|entry| entry.summary.id == id);
                entry.map(|entry| {
                    entry.summary.pinned = !entry.summary.pinned;
                    if entry.summary.pinned {
                        self.pins.insert(id.clone());
                    } else {
                        self.pins.remove(&id);
                    }
                    Event::SetPinned { id, pinned: entry.summary.pinned }
                })
            }
            Message::Delete(id) => Some(Event::DeleteSession(id)),
        };
        (Task::none(), event)
    }

    /// Draws the sidebar.
    pub fn view(&self) -> Element<'_, Message> {
        self.view_with(|message| message, None)
    }

    /// Draws the sidebar in the app's message type. `project_files` is drawn
    /// directly under the highlighted project's row: the project whose page
    /// is open, else the one set by `set_active_project`, else the active
    /// session's project. The search box does not filter it.
    pub fn view_with<'a, M: 'a>(
        &'a self,
        map: impl Fn(Message) -> M + Clone + 'a,
        mut project_files: Option<Element<'a, M>>,
    ) -> Element<'a, M> {
        let plus = container(tinted(Icon::Plus, T::Label, Tone::Normal)).center(sp(Sp::Xl)).style(style::fill(C::Accent, tokens().radius(RadiusRole::Pill)));
        let new = row![plus, label("New session", T::Body, Tone::Normal).width(Fill), label("Ctrl N", T::Caption, Tone::Faint)];
        let new = button(new.spacing(sp(Sp::S)).align_y(Alignment::Center))
            .on_press(Message::New)
            .style(style::button(ButtonKind::Row { selected: false }))
            .padding(row_pad())
            .width(Fill);

        let search = text_input("Search", &self.query)
            .id(SEARCH_ID)
            .on_input(Message::Search)
            .icon(text_input::Icon {
                font: iced::Font::default(),
                code_point: '⌕',
                size: Some(ts(T::Body).into()),
                spacing: sp(Sp::S),
                side: text_input::Side::Left,
            })
            .size(ts(T::Body))
            .style(style::input)
            .padding(row_pad());

        let mut lists = Column::new();
        if self.machines_open {
            lists = lists.push(self.machine_list());
        }

        let mut segments: Vec<Element<'a, M>> = Vec::new();
        let projects = self.projects();
        let active_project = self
            .shown_project
            .as_deref()
            .or(self.active_project.as_deref())
            .or_else(|| self.active.as_deref().and_then(|id| self.session(id)).and_then(|s| s.project.as_deref()));
        lists = lists.push(header("Projects"));
        if projects.is_empty() {
            lists = lists.push(hint(if self.projects.is_empty() { "No projects yet" } else { "No match" }));
        }
        for project in projects {
            let is_active = active_project == Some(project);
            let mut line = row![tinted(Icon::Folder, T::Body, Tone::Muted), label(project, T::Body, Tone::Normal).width(Fill)].spacing(sp(Sp::S)).align_y(Alignment::Center);
            if self.pinned_projects.contains(project) {
                line = line.push(tinted(Icon::Pin, T::Caption, Tone::Muted));
            }
            let body = button(line)
                .on_press(Message::OpenProject(project.to_owned()))
                .style(style::button(ButtonKind::Row { selected: is_active }))
                .padding(row_pad())
                .width(Fill);
            let pin = row_action(Icon::Pin, Message::ToggleProjectPin(project.to_owned()));
            lists = lists.push(hover(body, row_actions([pin], is_active)));
            if is_active && let Some(files) = project_files.take() {
                segments.push(Element::from(std::mem::replace(&mut lists, Column::new()).spacing(sp(Sp::Xs) / 2.0)).map(map.clone()));
                segments.push(files);
            }
        }

        let pinned = self.pinned();
        if !pinned.is_empty() {
            lists = lists.push(header("Pinned"));
            for session in pinned {
                lists = lists.push(self.session_row(session));
            }
        }

        let recents = self.recents();
        lists = lists.push(header("Recents"));
        if recents.is_empty() {
            lists = lists.push(hint(if self.query.is_empty() { "No sessions on this machine" } else { "No match" }));
        }
        for session in recents {
            lists = lists.push(self.session_row(session));
        }

        segments.push(Element::from(lists.spacing(sp(Sp::Xs) / 2.0)).map(map.clone()));
        let content = column![
            Element::from(self.machine_switcher()).map(map.clone()),
            Element::from(new).map(map.clone()),
            Element::from(search).map(map.clone()),
            scrollable(Column::with_children(segments)).height(Fill),
            Element::from(self.footer()).map(map),
        ]
        .spacing(sp(Sp::Xs))
        .padding(sp(Sp::S));

        container(content).style(style::container(Surface::Side)).width(Fill).height(Fill).into()
    }

    fn node_label(node: &NodeStatus) -> &'static str {
        match (node.local, node.online) {
            (true, _) => "This computer",
            (false, true) => "Peer · online",
            (false, false) => "Peer · offline",
        }
    }

    fn machine_switcher(&self) -> Element<'_, Message> {
        let machine = self.machine();
        let (name, sub, online) = match &machine {
            NodeFilter::All => ("All machines".to_owned(), "Recents from every machine".to_owned(), true),
            NodeFilter::Node(key) => match self.nodes.get(key) {
                Some(node) => (node.name.clone(), Self::node_label(node).to_owned(), node.local || node.online),
                None => (self.nodes.name_of(key).to_owned(), "Unknown machine".to_owned(), false),
            },
        };
        let chevron = if self.machines_open { Icon::ChevronUp } else { Icon::ChevronDown };
        let bold = iced::Font { weight: iced::font::Weight::Bold, ..tokens().font(T::Body) };
        let body = button(
            row![
                status_dot(online),
                column![label(name, T::Body, Tone::Normal).font(bold), label(sub, T::Caption, Tone::Muted)].width(Fill),
                tinted(chevron, T::Body, Tone::Muted),
            ]
            .spacing(sp(Sp::S))
            .align_y(Alignment::Center),
        )
        .on_press(Message::ToggleMachines)
        .style(style::button(ButtonKind::Row { selected: self.machines_open }))
        .padding(row_pad())
        .width(Fill);
        container(body).id(MACHINE_SWITCHER_ID).into()
    }

    fn machine_list(&self) -> Element<'_, Message> {
        let current = self.machine();
        let mut list = Column::new().spacing(sp(Sp::Xs) / 2.0);
        let mut options = vec![(NodeFilter::All, "All machines".to_owned(), "Recents from every machine".to_owned(), true)];
        for node in self.nodes.iter() {
            options.push((NodeFilter::Node(node.key.clone()), node.name.clone(), Self::node_label(node).to_owned(), node.local || node.online));
        }
        for (filter, name, sub, online) in options {
            let (count, waiting) = self.counts(&filter);
            let tally = if waiting > 0 {
                label(format!("{waiting} need you"), T::Caption, Tone::Warn)
            } else {
                label(format!("{count} sessions"), T::Caption, Tone::Faint)
            };
            let selected = filter == current;
            let line = row![status_dot(online), column![label(name, T::Body, Tone::Normal), label(sub, T::Caption, Tone::Faint)].width(Fill), tally];
            list = list.push(
                button(line.spacing(sp(Sp::S)).align_y(Alignment::Center))
                    .on_press(Message::PickMachine(filter))
                    .style(style::button(ButtonKind::Row { selected }))
                    .padding(row_pad())
                    .width(Fill),
            );
        }
        list = list.push(
            button(label("Manage peers", T::Label, Tone::Normal)).on_press(Message::OpenPeers).style(style::button(ButtonKind::Ghost)).padding(row_pad()),
        );
        container(list).style(style::container(Surface::Popup)).padding(sp(Sp::Xs)).into()
    }

    fn footer(&self) -> Element<'_, Message> {
        let mut footer = Column::new().spacing(sp(Sp::Xs) / 2.0);
        if self.updates > 0 {
            footer = footer.push(footer_item(Icon::ArrowUp, "Updates", Some(label(self.updates.to_string(), T::Caption, Tone::Warn).into()), Message::OpenUpdates));
        }
        let meter: Option<Element<'_, Message>> = self.usage.as_ref().map(|usage| {
            let fraction = usage.fraction.clamp(0.0, 1.0);
            row![
                container(meter(fraction)).width(sp(Sp::Xl) * 2.0),
                label(format!("{} {:.0}%", usage.label, fraction * 100.0), T::Caption, Tone::Faint),
            ]
            .spacing(sp(Sp::S))
            .align_y(Alignment::Center)
            .into()
        });
        footer = footer.push(footer_item(Icon::Gauge, "Usage", meter, Message::OpenUsage));
        footer = footer.push(footer_item(Icon::SlidersHorizontal, "Configs", None, Message::OpenConfigs));
        footer = footer.push(footer_item(Icon::Settings, "Settings", None, Message::OpenSettings));
        column![rule::horizontal(1).style(style::rule), footer].spacing(sp(Sp::Xs)).into()
    }

    fn session_row<'a>(&'a self, session: &'a SessionSummary) -> Element<'a, Message> {
        let selected = self.shown_project.is_none() && self.active.as_deref() == Some(session.id.as_str());

        // Clipped, so a long title ends at its own edge instead of running
        // under "Needs you" or the machine name.
        let title = container(label(session.title.as_str(), T::Body, Tone::Normal).wrapping(text::Wrapping::None)).width(Fill).clip(true);
        let mut line = row![title].spacing(sp(Sp::S)).align_y(Alignment::Center);
        if session.waiting {
            line = line.push(container(label("Needs you", T::Caption, Tone::Warn)).id(waiting_badge_id(&session.id)));
        }
        if self.machine() == NodeFilter::All {
            line = line.push(label(self.nodes.name_of(&session.node), T::Caption, Tone::Faint));
        }

        let body = button(line)
            .on_press(Message::OpenSession(session.id.clone()))
            .style(style::button(ButtonKind::Row { selected }))
            .padding(row_pad())
            .width(Fill);
        let pin = row_action(Icon::Pin, Message::TogglePin(session.id.clone()));
        let delete = row_action(Icon::X, Message::Delete(session.id.clone()));
        hover(body, row_actions([pin, delete], selected))
    }
}

/// Padding of a sidebar row.
fn row_pad() -> Padding { Padding::from([sp(Sp::Xs) * 1.75, sp(Sp::S) * 1.25]) }

fn status_dot<'a>(online: bool) -> Element<'a, Message> {
    let d = sp(Sp::S);
    container(space()).width(d).height(d).style(style::fill(if online { C::Ok } else { C::Text3 }, tokens().radius(RadiusRole::Dot))).into()
}

fn row_action<'a>(glyph: Icon, on: Message) -> Element<'a, Message> {
    button(tinted(glyph, T::Label, Tone::Muted)).on_press(on).style(style::button(ButtonKind::Ghost)).padding(sp(Sp::Xs)).into()
}

/// Hover actions on a backing the colour of the hovered (or selected) row, so the
/// icons cover the end of the title instead of mixing with it.
fn row_actions<'a>(actions: impl IntoIterator<Item = Element<'a, Message>>, selected: bool) -> Element<'a, Message> {
    let half = sp(Sp::Xs) / 2.0;
    let actions = container(row(actions).spacing(half))
        .padding(Padding::ZERO.left(half).right(half))
        .style(style::fill(if selected { C::Selected } else { C::Hover }, 0.0));
    container(actions).align_right(Fill).center_y(Fill).padding(pad_x(Sp::Xs)).into()
}

fn footer_item<'a>(glyph: Icon, text: &'a str, trailing: Option<Element<'a, Message>>, on_press: Message) -> Element<'a, Message> {
    let mut line = row![tinted(glyph, T::Body, Tone::Muted), label(text, T::Body, Tone::Normal).width(Fill)].spacing(sp(Sp::S)).align_y(Alignment::Center);
    if let Some(trailing) = trailing {
        line = line.push(trailing);
    }
    button(line).on_press(on_press).style(style::button(ButtonKind::Row { selected: false })).padding(row_pad()).width(Fill).into()
}

fn header(text: &str) -> Element<'_, Message> {
    container(label(text, T::Caption, Tone::Faint)).padding(Padding::from([sp(Sp::Xs), row_pad().left]).top(sp(Sp::M))).into()
}

fn hint(text: &str) -> Element<'_, Message> {
    container(label(text, T::Label, Tone::Faint)).padding(Padding::from([sp(Sp::Xs), row_pad().left])).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::NodeStatus;

    fn summary(id: &str, title: &str, node: &str, updated: i64) -> SessionSummary {
        SessionSummary {
            id: id.into(),
            title: title.into(),
            project: None,
            node: node.into(),
            pinned: false,
            waiting: false,
            updated,
        }
    }

    fn nodes() -> Nodes {
        Nodes::new(vec![
            NodeStatus { key: "k-desk".into(), name: "desk".into(), online: true, local: true },
            NodeStatus { key: "k-laptop".into(), name: "laptop".into(), online: false, local: false },
        ])
    }

    fn ids(list: Vec<&SessionSummary>) -> Vec<&str> {
        list.into_iter().map(|summary| summary.id.as_str()).collect()
    }

    #[test]
    fn recents_merge_nodes_newest_first_and_skip_pinned() {
        let mut state = State::default();
        state.set_nodes(nodes());
        let _ = state.update(Message::PickMachine(NodeFilter::All));
        let mut pinned_old = summary("p", "Pinned", "k-desk", 10);
        pinned_old.pinned = true;
        state.set_sessions([
            summary("a", "Desk old", "k-desk", 100),
            summary("b", "Laptop new", "k-laptop", 300),
            summary("c", "Desk mid", "k-desk", 200),
            pinned_old,
        ]);
        assert_eq!(ids(state.recents()), ["b", "c", "a"]);
        assert_eq!(ids(state.pinned()), ["p"]);
    }

    #[test]
    fn pinned_sorts_newest_first_and_pin_toggle_moves_between_sections() {
        let mut state = State::default();
        let mut x = summary("x", "X", "k-desk", 50);
        x.pinned = true;
        let mut y = summary("y", "Y", "k-desk", 80);
        y.pinned = true;
        state.set_sessions([x, y, summary("z", "Z", "k-desk", 10)]);
        assert_eq!(ids(state.pinned()), ["y", "x"]);

        let (_, event) = state.update(Message::TogglePin("z".into()));
        assert_eq!(event, Some(Event::SetPinned { id: "z".into(), pinned: true }));
        assert_eq!(ids(state.pinned()), ["y", "x", "z"]);
        assert!(state.recents().is_empty());

        let (_, event) = state.update(Message::TogglePin("y".into()));
        assert_eq!(event, Some(Event::SetPinned { id: "y".into(), pinned: false }));
        assert_eq!(ids(state.recents()), ["y"]);
    }

    #[test]
    fn pins_survive_a_fresh_summary_and_apply_to_sessions_listed_later() {
        let mut state = State::default();
        state.set_pins(["later".to_owned()]);
        state.set_sessions([summary("a", "A", "k-desk", 1)]);
        let _ = state.update(Message::TogglePin("a".into()));
        // The app replaces the row after every turn with a summary that
        // doesn't know about pins.
        state.upsert_session(summary("a", "A renamed", "k-desk", 2));
        state.upsert_session(summary("later", "Later", "k-laptop", 3));
        assert_eq!(ids(state.pinned()), ["later", "a"]);
        assert_eq!(state.pins().iter().map(String::as_str).collect::<Vec<_>>(), ["a", "later"]);
        let _ = state.update(Message::TogglePin("later".into()));
        assert_eq!(state.pins().iter().map(String::as_str).collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn search_filters_by_title_project_and_node_name_case_insensitively() {
        let mut state = State::default();
        state.set_nodes(nodes());
        let _ = state.update(Message::PickMachine(NodeFilter::All));
        let mut in_project = summary("p", "Refactor", "k-desk", 1);
        in_project.project = Some("Harness".into());
        state.set_sessions([summary("a", "Fix Deploy script", "k-desk", 3), summary("b", "Notes", "k-laptop", 2), in_project]);
        state.set_projects(["Harness".to_owned(), "Website".to_owned()]);

        search(&mut state, "deploy");
        assert_eq!(ids(state.recents()), ["a"]);

        search(&mut state, "LAPTOP");
        assert_eq!(ids(state.recents()), ["b"]);

        search(&mut state, "harness refac");
        assert_eq!(ids(state.recents()), ["p"]);
        assert_eq!(state.projects(), Vec::<&str>::new());

        search(&mut state, "web");
        assert_eq!(state.projects(), ["Website"]);

        search(&mut state, "");
        assert_eq!(state.recents().len(), 3);
    }

    fn search(state: &mut State, query: &str) {
        let (_, event) = state.update(Message::Search(query.to_owned()));
        assert_eq!(event, None);
    }

    #[test]
    fn renaming_a_node_updates_search_tags() {
        let mut state = State::default();
        state.set_sessions([summary("a", "Thing", "k-new", 1)]);
        search(&mut state, "studio");
        assert!(state.recents().is_empty());

        state.set_nodes(Nodes::new(vec![NodeStatus {
            key: "k-new".into(),
            name: "Studio".into(),
            online: true,
            local: false,
        }]));
        assert_eq!(ids(state.recents()), ["a"]);
    }

    #[test]
    fn upsert_replaces_by_id_and_opening_highlights() {
        let mut state = State::default();
        state.set_sessions([summary("a", "Old title", "k", 1)]);
        let mut waiting = summary("a", "New title", "k", 5);
        waiting.waiting = true;
        state.upsert_session(waiting);
        assert_eq!(state.recents().len(), 1);
        assert!(state.session("a").is_some_and(|s| s.waiting && s.title == "New title"));

        let (_, event) = state.update(Message::OpenSession("a".into()));
        assert_eq!(event, Some(Event::OpenSession("a".into())));
        assert_eq!(state.active(), Some("a"));
    }

    #[test]
    fn machine_switcher_defaults_to_this_computer_and_filters_recents() {
        let mut state = State::default();
        state.set_nodes(nodes());
        state.set_sessions([summary("a", "Desk", "k-desk", 2), summary("b", "Laptop", "k-laptop", 1)]);
        assert_eq!(state.machine(), NodeFilter::Node("k-desk".into()));
        assert_eq!(ids(state.recents()), ["a"]);

        let _ = state.update(Message::ToggleMachines);
        assert!(state.machines_open());
        let _ = state.update(Message::PickMachine(NodeFilter::Node("k-laptop".into())));
        assert!(!state.machines_open(), "picking closes the list");
        assert_eq!(ids(state.recents()), ["b"]);

        let _ = state.update(Message::PickMachine(NodeFilter::All));
        assert_eq!(ids(state.recents()), ["a", "b"]);
    }

    #[test]
    fn footer_items_raise_their_events() {
        let mut state = State::default();
        for (message, event) in [
            (Message::OpenUsage, Event::OpenUsage),
            (Message::OpenConfigs, Event::OpenConfigs),
            (Message::OpenSettings, Event::OpenSettings),
        ] {
            assert_eq!(state.update(message).1, Some(event));
        }
    }
}
