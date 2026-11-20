//! The left sidebar.
//!
//! From the top: a machine switcher that says which machine you are working
//! on and lists your peers, New session, Search, Projects, Pinned and
//! Recents for the chosen machine, and a footer with Usage, Configs and
//! Settings. Sessions with an open decision say so in the needs-you color.

use std::collections::BTreeSet;

use crate::data::{NodeFilter, NodeStatus, Nodes, SessionSummary, UsageMeter};
use crate::theme::style;
use iced::widget::{Column, button, column, container, hover, row, scrollable, space, text, text_input};
use iced::{Alignment, Element, Fill, Length, Padding, Task};
use loa_ui_icons::{Icon, icon, muted};

/// Id of the search input, for focusing it from a shortcut.
pub const SEARCH_ID: &str = "loa-sidebar-search";

/// Container id of the machine switcher.
pub const MACHINE_SWITCHER_ID: &str = "loa-sidebar-machine";

/// Container id of the waiting label on a session row.
pub fn waiting_badge_id(session: &str) -> String {
    format!("loa-sidebar-waiting-{session}")
}

/// Recents rows drawn at first, and added by each "Show more".
pub const RECENTS_PAGE: usize = 50;

/// Sessions listed under the highlighted project; the rest are on its page.
pub const PROJECT_SESSIONS: usize = 8;

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
    /// `query` in lowercase, matched against the haystacks.
    lower_query: String,
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
    /// The machine whose sessions are listed: `machine` resolved against the nodes.
    filter: NodeFilter,
    /// Indices into `sessions` of the listed pinned and unpinned sessions,
    /// newest first. Kept in step by every change to their inputs, so drawing
    /// neither sorts nor lowercases.
    pinned_order: Vec<usize>,
    recent_order: Vec<usize>,
    /// Indices into `projects` of the listed projects, pinned ones first.
    visible_projects: Vec<usize>,
    /// "Show more" pages of Recents opened beyond the first.
    recents_pages: usize,
    /// Whether the machine list is open.
    machines_open: bool,
    /// The footer usage meter, once usage is known.
    usage: Option<UsageMeter>,
    /// The session whose "Move to project" list is open.
    move_menu: Option<String>,
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
    /// The row under Recents that lists the next page of sessions.
    ShowMoreRecents,
    /// The `+` in the Projects header.
    NewProject,
    /// The folder button on a session row: opens or closes its project list.
    ToggleMoveMenu(String),
    /// A project picked in a session's list; `None` is "No project".
    MoveTo { id: String, project: Option<String> },
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
    /// Open the New project screen.
    NewProject,
    /// Move a session to a project, or out of every project with `None`.
    MoveSession { id: String, project: Option<String> },
}

impl State {
    /// Replaces every session.
    pub fn set_sessions(&mut self, sessions: impl IntoIterator<Item = SessionSummary>) {
        let entries = sessions.into_iter().map(|summary| self.entry(summary)).collect();
        self.sessions = entries;
        self.reorder();
    }

    /// Adds a session or replaces the one with the same id. Its pinned flag
    /// comes from the pins; a summary that says pinned adds a pin.
    pub fn upsert_session(&mut self, summary: SessionSummary) {
        let entry = self.entry(summary);
        match self.sessions.iter_mut().find(|existing| existing.summary.id == entry.summary.id) {
            Some(existing) => *existing = entry,
            None => self.sessions.push(entry),
        }
        self.reorder();
    }

    /// [`Self::upsert_session`] for many sessions, sorting once.
    pub fn upsert_sessions(&mut self, summaries: impl IntoIterator<Item = SessionSummary>) {
        let mut index: std::collections::HashMap<String, usize> =
            self.sessions.iter().enumerate().map(|(at, entry)| (entry.summary.id.clone(), at)).collect();
        for summary in summaries {
            let entry = self.entry(summary);
            match index.get(&entry.summary.id) {
                Some(&at) => self.sessions[at] = entry,
                None => {
                    index.insert(entry.summary.id.clone(), self.sessions.len());
                    self.sessions.push(entry);
                }
            }
        }
        self.reorder();
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
        self.reorder();
    }

    /// Ids of the pinned sessions, listed or not.
    pub fn pins(&self) -> &BTreeSet<String> {
        &self.pins
    }

    /// Drops a session.
    pub fn remove_session(&mut self, id: &str) {
        self.sessions.retain(|entry| entry.summary.id != id);
        self.reorder();
    }

    /// The session with this id.
    pub fn session(&self, id: &str) -> Option<&SessionSummary> {
        self.sessions.iter().map(|entry| &entry.summary).find(|summary| summary.id == id)
    }

    /// Replaces the pinned project names, as saved by the app.
    pub fn set_pinned_projects(&mut self, pinned: impl IntoIterator<Item = String>) {
        self.pinned_projects = pinned.into_iter().collect();
        self.refilter_projects();
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
        self.refilter_projects();
    }

    /// Replaces the node directory and refreshes node tags.
    pub fn set_nodes(&mut self, nodes: Nodes) {
        self.nodes = nodes;
        let sessions = std::mem::take(&mut self.sessions);
        self.sessions = sessions.into_iter().map(|entry| Entry::new(entry.summary, &self.nodes)).collect();
        self.reorder();
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

    /// The project whose page is open.
    pub fn shown_project(&self) -> Option<&str> {
        self.shown_project.as_deref()
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
        self.filter.clone()
    }

    /// Whether the machine list is open.
    pub fn machines_open(&self) -> bool {
        self.machines_open
    }

    /// Pinned sessions on the chosen machine that match the search, most
    /// recently updated first.
    pub fn pinned(&self) -> Vec<&SessionSummary> {
        self.pinned_order.iter().map(|&index| &self.sessions[index].summary).collect()
    }

    /// Unpinned sessions on the chosen machine that match the search, most
    /// recently updated first. The view draws the first pages of them.
    pub fn recents(&self) -> Vec<&SessionSummary> {
        self.recent_order.iter().map(|&index| &self.sessions[index].summary).collect()
    }

    /// Projects whose name matches the search: pinned ones by name, then the
    /// rest in the order given.
    pub fn projects(&self) -> Vec<&str> {
        self.visible_projects.iter().map(|&index| self.projects[index].as_str()).collect()
    }

    /// How many Recents rows the view draws.
    fn recents_limit(&self) -> usize {
        RECENTS_PAGE * (self.recents_pages + 1)
    }

    /// Resolves the machine filter and rebuilds the Pinned and Recents
    /// orders. A different machine starts Recents at one page again.
    fn reorder(&mut self) {
        let filter = match &self.machine {
            Some(machine) => machine.clone(),
            None => self.nodes.iter().find(|node| node.local).map_or(NodeFilter::All, |local| NodeFilter::Node(local.key.clone())),
        };
        if filter != self.filter {
            self.filter = filter;
            self.recents_pages = 0;
        }
        let terms: Vec<&str> = self.lower_query.split_whitespace().collect();
        let (mut pinned, mut recent) = (Vec::new(), Vec::new());
        for (index, entry) in self.sessions.iter().enumerate() {
            let on_machine = match &self.filter {
                NodeFilter::All => true,
                NodeFilter::Node(key) => entry.summary.node == *key,
            };
            if on_machine && terms.iter().all(|term| entry.haystack.contains(term)) {
                if entry.summary.pinned { pinned.push(index) } else { recent.push(index) }
            }
        }
        let newest = |a: &usize, b: &usize| {
            let (a, b) = (&self.sessions[*a].summary, &self.sessions[*b].summary);
            b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id))
        };
        pinned.sort_by(newest);
        recent.sort_by(newest);
        self.pinned_order = pinned;
        self.recent_order = recent;
    }

    /// Rebuilds the listed projects from the names, pins and search.
    fn refilter_projects(&mut self) {
        let terms: Vec<&str> = self.lower_query.split_whitespace().collect();
        let pinned = |index: &usize| self.pinned_projects.contains(&self.projects[*index]);
        let mut first: Vec<usize> = (0..self.projects.len()).filter(pinned).collect();
        first.sort_by(|a, b| self.projects[*a].cmp(&self.projects[*b]));
        let visible: Vec<usize> = first
            .into_iter()
            .chain((0..self.projects.len()).filter(|index| !pinned(index)))
            .filter(|index| {
                let name = self.projects[*index].to_lowercase();
                terms.iter().all(|term| name.contains(term))
            })
            .collect();
        self.visible_projects = visible;
    }

    /// Sessions and waiting sessions on a machine, for the switcher.
    fn counts(&self, filter: &NodeFilter) -> (usize, usize) {
        self.sessions
            .iter()
            .map(|entry| &entry.summary)
            .filter(|summary| match filter {
                NodeFilter::All => true,
                NodeFilter::Node(key) => &summary.node == key,
            })
            .fold((0, 0), |(count, waiting), summary| (count + 1, waiting + usize::from(summary.waiting)))
    }

    /// Handles a message.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::New => Some(Event::NewSession),
            Message::Search(query) => {
                self.lower_query = query.to_lowercase();
                self.query = query;
                self.reorder();
                self.refilter_projects();
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
                self.reorder();
                None
            }
            Message::ToggleProjectPin(project) => {
                let pinned = !self.pinned_projects.remove(&project);
                if pinned {
                    self.pinned_projects.insert(project.clone());
                }
                self.refilter_projects();
                Some(Event::SetProjectPinned { project, pinned })
            }
            Message::OpenSession(id) => {
                self.active = Some(id.clone());
                Some(Event::OpenSession(id))
            }
            Message::TogglePin(id) => {
                let entry = self.sessions.iter_mut().find(|entry| entry.summary.id == id);
                let event = entry.map(|entry| {
                    entry.summary.pinned = !entry.summary.pinned;
                    if entry.summary.pinned {
                        self.pins.insert(id.clone());
                    } else {
                        self.pins.remove(&id);
                    }
                    Event::SetPinned { id, pinned: entry.summary.pinned }
                });
                self.reorder();
                event
            }
            Message::Delete(id) => Some(Event::DeleteSession(id)),
            Message::ShowMoreRecents => {
                self.recents_pages += 1;
                None
            }
            Message::NewProject => Some(Event::NewProject),
            Message::ToggleMoveMenu(id) => {
                self.move_menu = (self.move_menu.as_deref() != Some(id.as_str())).then_some(id);
                None
            }
            Message::MoveTo { id, project } => {
                self.move_menu = None;
                Some(Event::MoveSession { id, project })
            }
        };
        (Task::none(), event)
    }

    /// Draws the sidebar.
    pub fn view(&self) -> Element<'_, Message> {
        self.view_with(|message| message)
    }

    /// Draws the sidebar in the app's message type. The highlighted project
    /// (the one whose page is open, else the one set by `set_active_project`,
    /// else the active session's project) lists its sessions under its row.
    pub fn view_with<'a, M: 'a>(&'a self, map: impl Fn(Message) -> M + Clone + 'a) -> Element<'a, M> {
        let new = button(
            row![
                container(icon(Icon::Plus, 13.0)).center(22).style(|theme: &iced::Theme| container::Style {
                    background: Some(style::tokens(theme).accent.into()),
                    border: iced::border::rounded(99),
                    ..container::Style::default()
                }),
                text("New session").size(14).width(Fill),
                text("Ctrl N").size(12).style(style::faint),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .on_press(Message::New)
        .style(style::row)
        .padding([7, 10])
        .width(Fill);

        let search = text_input("Search", &self.query)
            .id(SEARCH_ID)
            .on_input(Message::Search)
            .icon(text_input::Icon {
                font: iced::Font::default(),
                code_point: '⌕',
                size: Some(14.0.into()),
                spacing: 8.0,
                side: text_input::Side::Left,
            })
            .size(14)
            .style(style::input)
            .padding([6, 10]);

        let mut lists = Column::new();
        if self.machines_open {
            lists = lists.push(self.machine_list());
        }

        let projects = self.visible_projects.iter().map(|&index| self.projects[index].as_str());
        let active_project = self
            .shown_project
            .as_deref()
            .or(self.active_project.as_deref())
            .or_else(|| self.active.as_deref().and_then(|id| self.session(id)).and_then(|s| s.project.as_deref()));
        // "Projects" where the other headers sit, with the `+` centered
        // beside it and clear of the scrollbar drawn over the list's edge.
        let add = button(muted(Icon::Plus, 14.0)).on_press(Message::NewProject).style(style::ghost_button).padding(5);
        lists = lists.push(
            row![text("Projects").size(12).style(style::faint).width(Fill), add]
                .align_y(Alignment::Center)
                .padding(Padding { top: 6.0, right: 12.0, bottom: 0.0, left: 10.0 }),
        );
        if self.visible_projects.is_empty() {
            lists = lists.push(hint(if self.projects.is_empty() { "No projects yet" } else { "No match" }));
        }
        for project in projects {
            let is_active = active_project == Some(project);
            let pinned = self.pinned_projects.contains(project);
            let mut line = row![muted(Icon::Folder, 15.0), text(project).size(14).width(Fill)].spacing(10).align_y(Alignment::Center);
            if pinned {
                line = line.push(muted(Icon::Pin, 12.0));
            }
            let body = button(line)
                .on_press(Message::OpenProject(project.to_owned()))
                .style(style::row_for(is_active))
                .padding([7, 10])
                .width(Fill);
            let pin = button(muted(Icon::Pin, 13.0))
                .on_press(Message::ToggleProjectPin(project.to_owned()))
                .style(style::ghost_button)
                .padding(4);
            let pin = container(pin)
                .padding([0, 2])
                .style(move |theme| style::row_actions(theme, is_active));
            lists = lists.push(hover(body, container(pin).align_right(Fill).center_y(Fill).padding([0, 6])));
            if is_active {
                lists = self.project_sessions(lists, project);
            }
        }

        let filter = &self.filter;
        if !self.pinned_order.is_empty() {
            lists = lists.push(header("Pinned"));
            for &index in &self.pinned_order {
                lists = lists.push(self.session_row(&self.sessions[index].summary, filter));
            }
        }

        lists = lists.push(header("Recents"));
        if self.recent_order.is_empty() {
            lists = lists.push(hint(if self.query.is_empty() { "No sessions on this machine" } else { "No match" }));
        }
        let limit = self.recents_limit();
        for &index in self.recent_order.iter().take(limit) {
            lists = lists.push(self.session_row(&self.sessions[index].summary, filter));
        }
        let hidden = self.recent_order.len().saturating_sub(limit);
        if hidden > 0 {
            lists = lists.push(
                button(text(format!("Show {} more", hidden.min(RECENTS_PAGE))).size(13).style(style::muted))
                    .on_press(Message::ShowMoreRecents)
                    .style(style::ghost_button)
                    .padding([6, 10])
                    .width(Fill),
            );
        }

        let content = column![
            Element::from(self.machine_switcher()).map(map.clone()),
            Element::from(new).map(map.clone()),
            Element::from(search).map(map.clone()),
            Element::from(scrollable(lists.spacing(2)).height(Fill)).map(map.clone()),
            Element::from(self.footer()).map(map),
        ]
        .spacing(4)
        .padding(Padding { top: 8.0, right: 8.0, bottom: 8.0, left: 8.0 });

        container(content).style(style::side).width(Fill).height(Fill).into()
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
        let body = button(
            row![
                container(space()).width(8).height(8).style(style::status_dot(online)),
                column![text(name).size(14).font(bold()), text(sub).size(12).style(style::muted)].width(Fill),
                muted(chevron, 14.0),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .on_press(Message::ToggleMachines)
        .style(style::row_for(self.machines_open))
        .padding([6, 10])
        .width(Fill);
        container(body).id(MACHINE_SWITCHER_ID).into()
    }

    fn machine_list(&self) -> Element<'_, Message> {
        let current = self.machine();
        let mut list = Column::new().spacing(2);
        let mut options = vec![(NodeFilter::All, "All machines".to_owned(), "Recents from every machine".to_owned(), true)];
        for node in self.nodes.iter() {
            options.push((NodeFilter::Node(node.key.clone()), node.name.clone(), Self::node_label(node).to_owned(), node.local || node.online));
        }
        for (filter, name, sub, online) in options {
            let (count, waiting) = self.counts(&filter);
            let tally = if waiting > 0 {
                text!("{waiting} need you").size(12).style(style::waiting_text)
            } else {
                text!("{count} sessions").size(12).style(style::faint)
            };
            let selected = filter == current;
            list = list.push(
                button(
                    row![
                        container(space()).width(8).height(8).style(style::status_dot(online)),
                        column![text(name).size(14), text(sub).size(12).style(style::faint)].width(Fill),
                        tally,
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                )
                .on_press(Message::PickMachine(filter))
                .style(style::row_for(selected))
                .padding([6, 10])
                .width(Fill),
            );
        }
        list = list.push(
            button(text("Manage peers").size(13)).on_press(Message::OpenPeers).style(style::ghost_button).padding([6, 10]),
        );
        container(list).style(style::window).padding(6).into()
    }

    fn footer(&self) -> Element<'_, Message> {
        let mut footer = Column::new().spacing(2);
        if self.updates > 0 {
            footer = footer.push(footer_item(
                Icon::ArrowUp,
                "Updates",
                Some(text(self.updates).size(12).style(style::waiting_text).into()),
                Message::OpenUpdates,
            ));
        }
        let meter: Option<Element<'_, Message>> = self.usage.as_ref().map(|usage| {
            let fraction = usage.fraction.clamp(0.0, 1.0);
            let filled = (fraction * 44.0).max(if fraction > 0.0 { 2.0 } else { 0.0 });
            row![
                container(container(space()).width(filled).height(4).style(style::meter_fill(fraction)))
                    .width(44)
                    .height(4)
                    .style(style::meter_track),
                text!("{} {:.0}%", usage.label, fraction * 100.0).size(12).style(style::faint),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
        });
        footer = footer.push(footer_item(Icon::Gauge, "Usage", meter, Message::OpenUsage));
        footer = footer.push(footer_item(Icon::SlidersHorizontal, "Configs", None, Message::OpenConfigs));
        footer = footer.push(footer_item(Icon::Settings, "Settings", None, Message::OpenSettings));
        column![iced::widget::rule::horizontal(1).style(style::divider), footer].spacing(6).into()
    }

    /// Pushes `project`'s sessions under its row, indented, pinned first then
    /// newest, at most [`PROJECT_SESSIONS`] with a row to the project's page
    /// for the rest.
    fn project_sessions<'a>(&'a self, mut lists: Column<'a, Message>, project: &'a str) -> Column<'a, Message> {
        let mut sessions = self
            .pinned_order
            .iter()
            .chain(&self.recent_order)
            .map(|&index| &self.sessions[index].summary)
            .filter(|session| session.project.as_deref() == Some(project));
        let indent = Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 16.0 };
        let mut shown = 0;
        for session in sessions.by_ref().take(PROJECT_SESSIONS) {
            lists = lists.push(container(self.session_row(session, &self.filter)).padding(indent));
            shown += 1;
        }
        if shown == 0 {
            return lists.push(container(hint("No sessions yet")).padding(indent));
        }
        let more = sessions.count();
        if more > 0 {
            lists = lists.push(
                container(
                    button(text(format!("All {} sessions", shown + more)).size(13).style(style::muted))
                        .on_press(Message::OpenProject(project.to_owned()))
                        .style(style::ghost_button)
                        .padding([6, 10])
                        .width(Fill),
                )
                .padding(indent),
            );
        }
        lists
    }

    fn session_row<'a>(&'a self, session: &'a SessionSummary, filter: &NodeFilter) -> Element<'a, Message> {
        let selected = self.shown_project.is_none() && self.active.as_deref() == Some(session.id.as_str());

        // Clipped, so a long title ends at its own edge instead of running
        // under "Needs you" or the machine name.
        let title = container(text(session.title.as_str()).size(14).wrapping(text::Wrapping::None)).width(Fill).clip(true);
        let mut line = row![title]
            .spacing(8)
            .align_y(Alignment::Center);
        if session.waiting {
            line = line.push(
                container(text("Needs you").size(12).style(style::waiting_text)).id(waiting_badge_id(&session.id)),
            );
        }
        if *filter == NodeFilter::All {
            line = line.push(text(self.nodes.name_of(&session.node)).size(12).style(style::faint));
        }

        let body = button(line)
            .on_press(Message::OpenSession(session.id.clone()))
            .style(style::row_for(selected))
            .padding([7, 10])
            .width(Fill);

        let pin = button(muted(Icon::Pin, 13.0))
            .on_press(Message::TogglePin(session.id.clone()))
            .style(style::ghost_button)
            .padding(4);

        let delete = button(muted(Icon::X, 13.0))
            .on_press(Message::Delete(session.id.clone()))
            .style(style::ghost_button)
            .padding(4);

        let move_to = button(muted(Icon::Folder, 13.0))
            .on_press(Message::ToggleMoveMenu(session.id.clone()))
            .style(style::ghost_button)
            .padding(4);

        // On a backing the color of the hovered (or selected) row, so the
        // icons cover the end of the title instead of mixing with it.
        let actions = container(row![move_to, pin, delete].spacing(2)).padding([0, 2]).style(move |theme| style::row_actions(theme, selected));
        let row = hover(body, container(actions).align_right(Fill).center_y(Fill).padding([0, 6]));
        if self.move_menu.as_deref() != Some(session.id.as_str()) {
            return row;
        }
        // The session's project list: "No project", then every project.
        let choice = |label: &'a str, project: Option<&'a String>| {
            let current = session.project.as_ref() == project;
            let mut line = row![text(label).size(13).width(Fill)].align_y(Alignment::Center);
            if current {
                line = line.push(muted(Icon::Check, 12.0));
            }
            button(line)
                .on_press(Message::MoveTo { id: session.id.clone(), project: project.cloned() })
                .style(style::row_for(current))
                .padding([4, 10])
                .width(Fill)
        };
        let list = std::iter::once(choice("No project", None)).chain(self.projects.iter().map(|project| choice(project, Some(project))));
        let menu = container(Column::with_children(list.map(Element::from)).spacing(1)).padding(Padding { top: 2.0, right: 0.0, bottom: 4.0, left: 16.0 });
        column![row, menu].into()
    }
}

fn footer_item<'a>(glyph: Icon, label: &'a str, trailing: Option<Element<'a, Message>>, on_press: Message) -> Element<'a, Message> {
    let mut line = row![muted(glyph, 15.0), text(label).size(14).width(Fill)].spacing(10).align_y(Alignment::Center);
    if let Some(trailing) = trailing {
        line = line.push(trailing);
    }
    button(line).on_press(on_press).style(style::row).padding([7, 10]).width(Length::Fill).into()
}

fn bold() -> iced::Font {
    iced::Font { weight: iced::font::Weight::Bold, ..iced::Font::default() }
}

fn header(label: &str) -> Element<'_, Message> {
    container(text(label).size(12).style(style::faint))
        .padding(Padding { top: 14.0, right: 10.0, bottom: 4.0, left: 10.0 })
        .into()
}

fn hint(label: &str) -> Element<'_, Message> {
    container(text(label).size(13).style(style::faint)).padding([4, 10]).into()
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

    #[test]
    fn recents_show_a_page_at_a_time_and_a_new_machine_starts_over() {
        let mut state = State::default();
        state.set_nodes(nodes());
        state.set_sessions((0..120).map(|n| summary(&format!("s{n}"), "Session", "k-desk", n)));
        assert_eq!(state.recents().len(), 120, "every match stays listed for navigation");
        assert_eq!(state.recents_limit(), RECENTS_PAGE);

        let _ = state.update(Message::ShowMoreRecents);
        assert_eq!(state.recents_limit(), 2 * RECENTS_PAGE);
        // A new summary keeps the pages the user opened.
        state.upsert_session(summary("s0", "Renamed", "k-desk", 500));
        assert_eq!(state.recents_limit(), 2 * RECENTS_PAGE);
        assert_eq!(ids(state.recents())[0], "s0", "the order follows the update");

        let _ = state.update(Message::PickMachine(NodeFilter::All));
        assert_eq!(state.recents_limit(), RECENTS_PAGE);
    }

    #[test]
    fn a_session_moves_to_a_project_or_out_of_every_project() {
        let mut state = State::default();
        state.set_projects(["alpha".to_owned(), "beta".to_owned()]);
        state.set_sessions([summary("a", "A", "k", 1)]);
        let _ = state.update(Message::ToggleMoveMenu("a".into()));
        assert_eq!(state.move_menu.as_deref(), Some("a"));
        let _ = state.view();
        let moved = state.update(Message::MoveTo { id: "a".into(), project: Some("beta".into()) }).1;
        assert_eq!(moved, Some(Event::MoveSession { id: "a".into(), project: Some("beta".into()) }));
        assert!(state.move_menu.is_none(), "picking closes the list");
        let _ = state.update(Message::ToggleMoveMenu("a".into()));
        let _ = state.update(Message::ToggleMoveMenu("a".into()));
        assert!(state.move_menu.is_none(), "the folder button closes it again");
        let out = state.update(Message::MoveTo { id: "a".into(), project: None }).1;
        assert_eq!(out, Some(Event::MoveSession { id: "a".into(), project: None }));
    }
}
