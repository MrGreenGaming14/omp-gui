//! The left sidebar: New, Search, Projects, Peers, Tools, Pinned and Recents.
//!
//! Recents hold sessions from every node, each tagged with the node's name.
//! Sessions with an open decision carry a waiting badge.

use std::collections::BTreeSet;

use crate::data::{Nodes, SessionSummary};
use crate::theme::style;
use iced::widget::{Column, button, column, container, hover, row, scrollable, space, text, text_input};
use iced::{Alignment, Element, Fill, Padding, Task};
use loa_ui_icons::{Icon, icon};

/// Id of the search input, for focusing it from a shortcut.
pub const SEARCH_ID: &str = "loa-sidebar-search";

/// Container id of the waiting badge on a session row.
pub fn waiting_badge_id(session: &str) -> String {
    format!("loa-sidebar-waiting-{session}")
}

/// Sidebar state: the sessions, projects and nodes the app passed in, the
/// search query and the highlighted session.
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
    /// Updates waiting for the user; the Updates row shows while non-zero.
    updates: usize,
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
    /// The Peers item was clicked.
    OpenPeers,
    /// The Tools item was clicked.
    OpenTools,
    /// The Updates item was clicked.
    OpenUpdates,
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

    /// Replaces the project list.
    pub fn set_projects(&mut self, projects: impl IntoIterator<Item = String>) {
        self.projects = projects.into_iter().collect();
    }

    /// Highlights a project row. While set it replaces the active session's
    /// project as the highlighted one.
    pub fn set_active_project(&mut self, project: Option<String>) {
        self.active_project = project;
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

    /// The search text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Pinned sessions that match the search, most recently updated first.
    pub fn pinned(&self) -> Vec<&SessionSummary> {
        self.section(true)
    }

    /// Unpinned sessions from every node that match the search, most recently
    /// updated first.
    pub fn recents(&self) -> Vec<&SessionSummary> {
        self.section(false)
    }

    /// Projects whose name matches the search: pinned ones first, by name,
    /// then the rest in the order given.
    pub fn projects(&self) -> Vec<&str> {
        let query = self.query.to_lowercase();
        let pinned = self.projects.iter().filter(|project| self.pinned_projects.contains(*project));
        let mut pinned: Vec<&String> = pinned.collect();
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
            .filter(|entry| entry.summary.pinned == pinned)
            .filter(|entry| query.split_whitespace().all(|term| entry.haystack.contains(term)))
            .map(|entry| &entry.summary)
            .collect();
        list.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
        list
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
            Message::OpenPeers => Some(Event::OpenPeers),
            Message::OpenTools => Some(Event::OpenTools),
            Message::OpenUpdates => Some(Event::OpenUpdates),
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
            Message::ToggleProjectPin(project) => {
                let pinned = !self.pinned_projects.remove(&project);
                if pinned {
                    self.pinned_projects.insert(project.clone());
                }
                Some(Event::SetProjectPinned { project, pinned })
            }
            Message::Delete(id) => Some(Event::DeleteSession(id)),
        };
        (Task::none(), event)
    }

    /// Draws the sidebar.
    pub fn view(&self) -> Element<'_, Message> {
        let new = button(
            container(row![icon(Icon::Plus, 14.0), text("New session").size(14)].spacing(8).align_y(Alignment::Center))
                .center_x(Fill),
        )
        .on_press(Message::New)
        .style(style::row)
        .padding([7, 8])
        .width(Fill);

        let search = text_input("Search sessions", &self.query)
            .id(SEARCH_ID)
            .on_input(Message::Search)
            .size(14)
            .style(style::input)
            .padding([3, 8]);

        let mut lists = Column::new();

        let projects = self.projects();
        let active_project = self
            .active_project
            .as_deref()
            .or_else(|| self.active.as_deref().and_then(|id| self.session(id)).and_then(|s| s.project.as_deref()));
        lists = lists.push(header("PROJECTS"));
        if projects.is_empty() {
            lists = lists.push(hint(if self.projects.is_empty() { "No projects yet" } else { "No match" }));
        }
        for project in projects {
            let body = button(row![icon(Icon::Folder, 14.0), text(project).size(14)].spacing(8).align_y(Alignment::Center))
                .on_press(Message::OpenProject(project.to_owned()))
                .style(style::row_for(active_project == Some(project)))
                .padding([6, 8])
                .width(Fill);
            let pin = button(text(if self.pinned_projects.contains(project) { "Unpin" } else { "Pin" }).size(12))
                .on_press(Message::ToggleProjectPin(project.to_owned()))
                .style(style::selected_row)
                .padding([1, 6]);
            lists = lists.push(hover(body, container(pin).align_right(Fill).center_y(Fill).padding([0, 6])));
        }

        lists = lists.push(header("PEERS"));
        if self.nodes.iter().next().is_none() {
            lists = lists.push(hint("No peers yet"));
        }
        for node in self.nodes.iter() {
            let online = node.local || node.online;
            let name = if node.local { text!("{} (this)", node.name) } else { text(node.name.as_str()) };
            let name = if online { name } else { name.style(style::muted) };
            lists = lists.push(
                button(
                    row![container(space()).width(6).height(6).style(style::status_dot(online)), name.size(14)]
                        .spacing(10)
                        .align_y(Alignment::Center),
                )
                .on_press(Message::OpenPeers)
                .style(style::row)
                .padding([6, 8])
                .width(Fill),
            );
        }

        lists = lists.push(space().height(4));
        lists = lists.push(
            button(
                container(
                    row![icon(Icon::SlidersHorizontal, 13.0), text("Tools").size(14)]
                        .spacing(6)
                        .align_y(Alignment::Center),
                )
                .center_x(Fill),
            )
            .on_press(Message::OpenTools)
            .style(style::row)
            .padding([6, 8])
            .width(Fill),
        );
        if self.updates > 0 {
            lists = lists.push(
                button(
                    container(
                        row![
                            icon(Icon::ArrowUp, 13.0),
                            text("Updates").size(14),
                            container(text(self.updates).size(12)).style(style::badge).padding([1, 6]),
                        ]
                        .spacing(6)
                        .align_y(Alignment::Center),
                    )
                    .center_x(Fill),
                )
                .on_press(Message::OpenUpdates)
                .style(style::row)
                .padding([6, 8])
                .width(Fill),
            );
        }

        let pinned = self.pinned();
        if !pinned.is_empty() {
            lists = lists.push(header("PINNED"));
            for session in pinned {
                lists = lists.push(self.session_row(session));
            }
        }

        let recents = self.recents();
        lists = lists.push(header("RECENTS"));
        if recents.is_empty() {
            lists = lists.push(hint(if self.query.is_empty() { "No sessions yet" } else { "No match" }));
        }
        for session in recents {
            lists = lists.push(self.session_row(session));
        }

        let content = column![new, search, scrollable(lists).height(Fill)].spacing(8).padding([6, 8]);

        container(content).style(style::chrome).width(Fill).height(Fill).into()
    }

    fn session_row<'a>(&'a self, session: &'a SessionSummary) -> Element<'a, Message> {
        let selected = self.active.as_deref() == Some(session.id.as_str());

        let mut line = row![text(session.title.as_str()).size(14).width(Fill).wrapping(text::Wrapping::None)]
            .spacing(6)
            .align_y(Alignment::Center);
        if session.waiting {
            line = line.push(
                container(text("waiting").size(12))
                    .id(waiting_badge_id(&session.id))
                    .style(style::waiting_badge)
                    .padding([1, 5]),
            );
        }
        let tag = text(self.nodes.name_of(&session.node)).size(12);
        line = line.push(container(tag).style(style::badge).padding([1, 5]));

        let body = button(line)
            .on_press(Message::OpenSession(session.id.clone()))
            .style(style::row_for(selected))
            .padding([6, 8])
            .width(Fill);

        let pin = button(text(if session.pinned { "Unpin" } else { "Pin" }).size(12))
            .on_press(Message::TogglePin(session.id.clone()))
            .style(style::selected_row)
            .padding([1, 6]);

        let delete = button(text("Delete").size(12))
            .on_press(Message::Delete(session.id.clone()))
            .style(style::selected_row)
            .padding([1, 6]);

        hover(body, container(row![pin, delete].spacing(4)).align_right(Fill).center_y(Fill).padding([0, 6]))
    }
}

fn header(label: &str) -> Element<'_, Message> {
    container(text(label).size(12).style(style::muted))
        .padding(Padding { top: 12.0, right: 8.0, bottom: 4.0, left: 8.0 })
        .into()
}

fn hint(label: &str) -> Element<'_, Message> {
    container(text(label).size(13).style(style::muted)).padding([4, 8]).into()
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
}
