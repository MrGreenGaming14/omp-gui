//! The left sidebar: a machine switcher, New session, Search, Projects, Pinned and Recents for
//! the chosen machine, and a footer with Updates, Usage, Configs and Settings.

use crate::data::{NodeFilter, NodeStatus, Nodes, SessionSummary, UsageMeter};
use iced::widget::{Column, button, column, container, hover, row, rule, scrollable, space, text, text_input};
use iced::{Alignment, Element, Fill, Padding, Task};
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{ColorRole as C, RadiusRole, SpaceRole as Sp, TextRole as T, pad_x, sp, text_size as ts, tokens};
use loa_ui_kit::widgets::{label, meter};
use std::collections::BTreeSet;

/// Id of the search input, for focusing it from a shortcut.
pub const SEARCH_ID: &str = "loa-sidebar-search";

#[derive(Debug, Clone, Default)]
pub struct State {
    /// Sessions with their lowercase search text, rebuilt when sessions or node names change.
    sessions: Vec<(SessionSummary, String)>,
    projects: Vec<String>,
    nodes: Nodes,
    /// Pinned session ids; app data that outlives the summaries passed in.
    pins: BTreeSet<String>,
    pinned_projects: BTreeSet<String>,
    query: String,
    active: Option<String>,
    /// The project the user selected; highlighted instead of the active session's.
    active_project: Option<String>,
    /// The project whose page fills the center.
    shown_project: Option<String>,
    updates: usize,
    /// The machine the user picked; `None` means this computer.
    machine: Option<NodeFilter>,
    machines_open: bool,
    usage: Option<UsageMeter>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    New,
    Search(String),
    OpenProject(String),
    OpenPeers,
    OpenTools,
    OpenUpdates,
    OpenUsage,
    OpenConfigs,
    OpenSettings,
    ToggleMachines,
    PickMachine(NodeFilter),
    OpenSession(String),
    TogglePin(String),
    ToggleProjectPin(String),
    Delete(String),
}

/// Navigation the app acts on; the frame forwards it as `layout::Event::Sidebar`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    NewSession,
    OpenSession(String),
    OpenProject(String),
    OpenPeers,
    OpenTools,
    OpenUpdates,
    OpenUsage,
    OpenConfigs,
    OpenSettings,
    /// Persist the pinned flag of a session.
    SetPinned { id: String, pinned: bool },
    /// Persist the pinned flag of a project.
    SetProjectPinned { project: String, pinned: bool },
    DeleteSession(String),
}

impl State {
    pub fn set_sessions(&mut self, sessions: impl IntoIterator<Item = SessionSummary>) { self.sessions = sessions.into_iter().map(|s| self.entry(s)).collect() }

    /// Adds or replaces a session; its pinned flag comes from the pins, and a pinned summary pins.
    pub fn upsert_session(&mut self, summary: SessionSummary) {
        let entry = self.entry(summary);
        match self.sessions.iter_mut().find(|(s, _)| s.id == entry.0.id) {
            Some(existing) => *existing = entry,
            None => self.sessions.push(entry),
        }
    }

    fn entry(&mut self, mut s: SessionSummary) -> (SessionSummary, String) {
        if s.pinned { self.pins.insert(s.id.clone()); } else { s.pinned = self.pins.contains(&s.id) }
        let haystack = [Some(s.title.as_str()), s.project.as_deref(), Some(self.nodes.name_of(&s.node))].into_iter().flatten().collect::<Vec<_>>().join("\n").to_lowercase();
        (s, haystack)
    }

    pub fn set_pins(&mut self, pins: impl IntoIterator<Item = String>) {
        self.pins = pins.into_iter().collect();
        self.sessions.iter_mut().for_each(|(s, _)| s.pinned = self.pins.contains(&s.id));
    }

    pub fn pins(&self) -> &BTreeSet<String> { &self.pins }

    pub fn remove_session(&mut self, id: &str) { self.sessions.retain(|(s, _)| s.id != id) }

    pub fn session(&self, id: &str) -> Option<&SessionSummary> { self.sessions.iter().map(|(s, _)| s).find(|s| s.id == id) }

    pub fn set_pinned_projects(&mut self, pinned: impl IntoIterator<Item = String>) { self.pinned_projects = pinned.into_iter().collect() }

    /// The pinned project names, including any no longer listed.
    pub fn pinned_projects(&self) -> &BTreeSet<String> { &self.pinned_projects }

    pub fn set_active_project(&mut self, project: Option<String>) { self.active_project = project }

    pub fn set_projects(&mut self, projects: impl IntoIterator<Item = String>) { self.projects = projects.into_iter().collect() }

    /// Replaces the node directory and refreshes the node names sessions are searched by.
    pub fn set_nodes(&mut self, nodes: Nodes) {
        self.nodes = nodes;
        let sessions = std::mem::take(&mut self.sessions);
        self.set_sessions(sessions.into_iter().map(|(s, _)| s));
    }

    pub fn nodes(&self) -> &Nodes { &self.nodes }

    pub fn set_active(&mut self, id: Option<String>) { self.active = id }

    pub fn set_shown_project(&mut self, project: Option<String>) { self.shown_project = project }

    pub fn active(&self) -> Option<&str> { self.active.as_deref() }

    /// Updates waiting for the user; the Updates row shows while non-zero.
    pub fn set_updates(&mut self, count: usize) { self.updates = count }

    pub fn updates(&self) -> usize { self.updates }

    pub fn set_usage(&mut self, usage: Option<UsageMeter>) { self.usage = usage }

    pub fn query(&self) -> &str { &self.query }

    /// The listed machine: the user's pick, else this computer, else every machine.
    pub fn machine(&self) -> NodeFilter {
        self.machine.clone().unwrap_or_else(|| self.nodes.iter().find(|n| n.local).map_or(NodeFilter::All, |n| NodeFilter::Node(n.key.clone())))
    }

    pub fn machines_open(&self) -> bool { self.machines_open }

    /// Pinned sessions on the chosen machine matching the search, newest first.
    pub fn pinned(&self) -> Vec<&SessionSummary> { self.section(true) }

    /// Unpinned sessions on the chosen machine matching the search, newest first.
    pub fn recents(&self) -> Vec<&SessionSummary> { self.section(false) }

    /// Projects matching the search: pinned ones sorted first, then the rest in order.
    pub fn projects(&self) -> Vec<&str> {
        let query = self.query.to_lowercase();
        let mut pinned: Vec<&String> = self.projects.iter().filter(|p| self.pinned_projects.contains(*p)).collect();
        pinned.sort();
        let rest = self.projects.iter().filter(|p| !self.pinned_projects.contains(*p));
        pinned.into_iter().chain(rest).filter(|p| query.split_whitespace().all(|t| p.to_lowercase().contains(t))).map(String::as_str).collect()
    }

    fn section(&self, pinned: bool) -> Vec<&SessionSummary> {
        let (query, machine) = (self.query.to_lowercase(), self.machine());
        let mut list: Vec<&SessionSummary> = self.sessions.iter()
            .filter(|(s, hay)| s.pinned == pinned && machine.matches(&s.node) && query.split_whitespace().all(|t| hay.contains(t)))
            .map(|(s, _)| s).collect();
        list.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
        list
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::New => Some(Event::NewSession),
            Message::Search(query) => {
                self.query = query;
                None
            }
            Message::OpenProject(p) => Some(Event::OpenProject(p)),
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
                (self.machine, self.machines_open) = (Some(filter), false);
                None
            }
            Message::ToggleProjectPin(project) => {
                let pinned = !self.pinned_projects.remove(&project);
                if pinned { self.pinned_projects.insert(project.clone()); }
                Some(Event::SetProjectPinned { project, pinned })
            }
            Message::OpenSession(id) => {
                self.active = Some(id.clone());
                Some(Event::OpenSession(id))
            }
            Message::TogglePin(id) => self.sessions.iter_mut().find(|(s, _)| s.id == id).map(|(s, _)| {
                s.pinned = !s.pinned;
                if s.pinned { self.pins.insert(id.clone()); } else { self.pins.remove(&id); }
                Event::SetPinned { id, pinned: s.pinned }
            }),
            Message::Delete(id) => Some(Event::DeleteSession(id)),
        };
        (Task::none(), event)
    }

    /// The sidebar in the app's message type. `project_files` is drawn under the highlighted
    /// project (the shown one, else the selected one, else the active session's); search
    /// does not filter it.
    pub fn view_with<'a, M: 'a>(&'a self, map: impl Fn(Message) -> M + Clone + 'a, mut project_files: Option<Element<'a, M>>) -> Element<'a, M> {
        let plus = container(tinted(Icon::Plus, T::Label, Tone::Normal)).center(sp(Sp::Xl)).style(style::fill(C::Accent, tokens().radius(RadiusRole::Pill)));
        let new = row![plus, label("New session", T::Body, Tone::Normal).width(Fill), label("Ctrl N", T::Caption, Tone::Faint)];
        let new = row_button(new.spacing(sp(Sp::S)).align_y(Alignment::Center), false, Message::New);
        let icon = text_input::Icon { font: iced::Font::default(), code_point: '⌕', size: Some(ts(T::Body).into()), spacing: sp(Sp::S), side: text_input::Side::Left };
        let search = text_input("Search", &self.query).id(SEARCH_ID).on_input(Message::Search).icon(icon).size(ts(T::Body)).style(style::input).padding(row_pad());

        let mut lists = Column::new().push(self.machines_open.then(|| self.machine_list()));
        let mut segments: Vec<Element<'a, M>> = Vec::new();
        let projects = self.projects();
        let active_session = self.active.as_deref().and_then(|id| self.session(id));
        let active_project = self.shown_project.as_deref().or(self.active_project.as_deref()).or_else(|| active_session.and_then(|s| s.project.as_deref()));
        lists = lists.push(header("Projects"));
        if projects.is_empty() { lists = lists.push(hint(if self.projects.is_empty() { "No projects yet" } else { "No match" })) }
        for project in projects {
            let on = active_project == Some(project);
            let pin = self.pinned_projects.contains(project).then(|| tinted(Icon::Pin, T::Caption, Tone::Muted));
            let line = row![tinted(Icon::Folder, T::Body, Tone::Muted), label(project, T::Body, Tone::Normal).width(Fill), pin].spacing(sp(Sp::S)).align_y(Alignment::Center);
            let body = row_button(line, on, Message::OpenProject(project.to_owned()));
            lists = lists.push(hover(body, row_actions([row_action(Icon::Pin, Message::ToggleProjectPin(project.to_owned()))], on)));
            if on && let Some(files) = project_files.take() {
                segments.push(Element::from(std::mem::replace(&mut lists, Column::new()).spacing(sp(Sp::Xs) / 2.0)).map(map.clone()));
                segments.push(files);
            }
        }
        let pinned = self.pinned();
        if !pinned.is_empty() { lists = lists.push(header("Pinned")).extend(pinned.into_iter().map(|s| self.session_row(s))) }
        let recents = self.recents();
        lists = lists.push(header("Recents"));
        if recents.is_empty() { lists = lists.push(hint(if self.query.is_empty() { "No sessions on this machine" } else { "No match" })) }
        lists = lists.extend(recents.into_iter().map(|s| self.session_row(s)));
        segments.push(Element::from(lists.spacing(sp(Sp::Xs) / 2.0)).map(map.clone()));

        let top = column![self.machine_switcher(), new, search].spacing(sp(Sp::Xs));
        let content = column![Element::from(top).map(map.clone()), scrollable(Column::with_children(segments)).height(Fill), Element::from(self.footer()).map(map)];
        container(content.spacing(sp(Sp::Xs)).padding(sp(Sp::S))).style(style::container(Surface::Side)).width(Fill).height(Fill).into()
    }

    /// Name, sub line and whether the dot shows online, for a machine choice.
    fn describe(&self, filter: &NodeFilter) -> (String, String, bool) {
        let node_label = |n: &NodeStatus| match (n.local, n.online) {
            (true, _) => "This computer",
            (false, true) => "Peer · online",
            (false, false) => "Peer · offline",
        };
        match filter {
            NodeFilter::All => ("All machines".into(), "Recents from every machine".into(), true),
            NodeFilter::Node(key) => match self.nodes.get(key) {
                Some(n) => (n.name.clone(), node_label(n).into(), n.local || n.online),
                None => (self.nodes.name_of(key).into(), "Unknown machine".into(), false),
            },
        }
    }

    fn machine_switcher(&self) -> Element<'_, Message> {
        let (name, sub, online) = self.describe(&self.machine());
        let bold = iced::Font { weight: iced::font::Weight::Bold, ..tokens().font(T::Body) };
        let chevron = tinted(if self.machines_open { Icon::ChevronUp } else { Icon::ChevronDown }, T::Body, Tone::Muted);
        let line = row![status_dot(online), column![label(name, T::Body, Tone::Normal).font(bold), label(sub, T::Caption, Tone::Muted)].width(Fill), chevron];
        row_button(line.spacing(sp(Sp::S)).align_y(Alignment::Center), self.machines_open, Message::ToggleMachines)
    }

    fn machine_list(&self) -> Element<'_, Message> {
        let current = self.machine();
        let filters = std::iter::once(NodeFilter::All).chain(self.nodes.iter().map(|n| NodeFilter::Node(n.key.clone())));
        let rows = filters.map(|filter| {
            let (name, sub, online) = self.describe(&filter);
            let on: Vec<_> = self.sessions.iter().filter(|(s, _)| filter.matches(&s.node)).collect();
            let waiting = on.iter().filter(|(s, _)| s.waiting).count();
            let tally = if waiting > 0 { label(format!("{waiting} need you"), T::Caption, Tone::Warn) } else { label(format!("{} sessions", on.len()), T::Caption, Tone::Faint) };
            let line = row![status_dot(online), column![label(name, T::Body, Tone::Normal), label(sub, T::Caption, Tone::Faint)].width(Fill), tally];
            row_button(line.spacing(sp(Sp::S)).align_y(Alignment::Center), filter == current, Message::PickMachine(filter))
        });
        let peers = button(label("Manage peers", T::Label, Tone::Normal)).on_press(Message::OpenPeers).style(style::button(ButtonKind::Ghost)).padding(row_pad());
        container(Column::with_children(rows).push(peers).spacing(sp(Sp::Xs) / 2.0)).style(style::container(Surface::Popup)).padding(sp(Sp::Xs)).into()
    }

    fn footer(&self) -> Element<'_, Message> {
        let updates = (self.updates > 0).then(|| footer_item(Icon::ArrowUp, "Updates", Some(label(self.updates.to_string(), T::Caption, Tone::Warn).into()), Message::OpenUpdates));
        let meter = self.usage.as_ref().map(|u| {
            let f = u.fraction.clamp(0.0, 1.0);
            row![container(meter(f)).width(sp(Sp::Xl) * 2.0), label(format!("{} {:.0}%", u.label, f * 100.0), T::Caption, Tone::Faint)].spacing(sp(Sp::S)).align_y(Alignment::Center).into()
        });
        let items = column![updates, footer_item(Icon::Gauge, "Usage", meter, Message::OpenUsage), footer_item(Icon::SlidersHorizontal, "Configs", None, Message::OpenConfigs), footer_item(Icon::Settings, "Settings", None, Message::OpenSettings)];
        column![rule::horizontal(1).style(style::rule), items.spacing(sp(Sp::Xs) / 2.0)].spacing(sp(Sp::Xs)).into()
    }

    fn session_row<'a>(&'a self, s: &'a SessionSummary) -> Element<'a, Message> {
        let selected = self.shown_project.is_none() && self.active.as_deref() == Some(s.id.as_str());
        // Clipped, so a long title ends at its own edge instead of running under the tags.
        let title = container(label(s.title.as_str(), T::Body, Tone::Normal).wrapping(text::Wrapping::None)).width(Fill).clip(true);
        let node = (self.machine() == NodeFilter::All).then(|| label(self.nodes.name_of(&s.node), T::Caption, Tone::Faint));
        let line = row![title, s.waiting.then(|| label("Needs you", T::Caption, Tone::Warn)), node].spacing(sp(Sp::S)).align_y(Alignment::Center);
        let actions = [row_action(Icon::Pin, Message::TogglePin(s.id.clone())), row_action(Icon::X, Message::Delete(s.id.clone()))];
        hover(row_button(line, selected, Message::OpenSession(s.id.clone())), row_actions(actions, selected))
    }
}

fn row_pad() -> Padding { Padding::from([sp(Sp::Xs) * 1.75, sp(Sp::S) * 1.25]) }

fn row_button<'a>(content: impl Into<Element<'a, Message>>, selected: bool, on: Message) -> Element<'a, Message> {
    button(content).on_press(on).style(style::button(ButtonKind::Row { selected })).padding(row_pad()).width(Fill).into()
}

fn status_dot<'a>(online: bool) -> Element<'a, Message> {
    let d = sp(Sp::S);
    container(space()).width(d).height(d).style(style::fill(if online { C::Ok } else { C::Text3 }, tokens().radius(RadiusRole::Dot))).into()
}

fn row_action<'a>(glyph: Icon, on: Message) -> Element<'a, Message> {
    button(tinted(glyph, T::Label, Tone::Muted)).on_press(on).style(style::button(ButtonKind::Ghost)).padding(sp(Sp::Xs)).into()
}

/// Hover actions on a backing the colour of the hovered (or selected) row, so they cover
/// the end of the title instead of mixing with it.
fn row_actions<'a>(actions: impl IntoIterator<Item = Element<'a, Message>>, selected: bool) -> Element<'a, Message> {
    let half = sp(Sp::Xs) / 2.0;
    let actions = container(row(actions).spacing(half)).padding(Padding::ZERO.left(half).right(half)).style(style::fill(if selected { C::Selected } else { C::Hover }, 0.0));
    container(actions).align_right(Fill).center_y(Fill).padding(pad_x(Sp::Xs)).into()
}

fn footer_item<'a>(glyph: Icon, text: &'a str, trailing: Option<Element<'a, Message>>, on: Message) -> Element<'a, Message> {
    row_button(row![tinted(glyph, T::Body, Tone::Muted), label(text, T::Body, Tone::Normal).width(Fill), trailing].spacing(sp(Sp::S)).align_y(Alignment::Center), false, on)
}

fn header(text: &str) -> Element<'_, Message> { container(label(text, T::Caption, Tone::Faint)).padding(Padding::from([sp(Sp::Xs), row_pad().left]).top(sp(Sp::M))).into() }

fn hint(text: &str) -> Element<'_, Message> { container(label(text, T::Label, Tone::Faint)).padding(Padding::from([sp(Sp::Xs), row_pad().left])).into() }

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, node: &str, updated: i64, pinned: bool) -> SessionSummary {
        SessionSummary { id: id.into(), title: format!("Title {id}"), project: None, node: node.into(), pinned, waiting: false, updated }
    }

    fn ids(list: Vec<&SessionSummary>) -> Vec<&str> { list.into_iter().map(|s| s.id.as_str()).collect() }

    fn nodes() -> Nodes {
        let node = |key: &str, name: &str, local| NodeStatus { key: key.into(), name: name.into(), online: local, local };
        Nodes::new(vec![node("k-desk", "desk", true), node("k-laptop", "laptop", false)])
    }

    #[test]
    fn machine_filter_pins_and_search_shape_the_lists() {
        let mut state = State::default();
        state.set_nodes(nodes());
        state.set_pins(["later".to_owned()]);
        let mut proj = summary("p", "k-desk", 5, false);
        (proj.title, proj.project) = ("Refactor".into(), Some("Harness".into()));
        state.set_sessions([summary("a", "k-desk", 100, false), summary("b", "k-laptop", 300, false), summary("c", "k-desk", 200, false), summary("x", "k-desk", 10, true), proj]);
        assert_eq!(state.machine(), NodeFilter::Node("k-desk".into()));
        assert_eq!((ids(state.recents()), ids(state.pinned())), (vec!["c", "a", "p"], vec!["x"]));
        let _ = state.update(Message::PickMachine(NodeFilter::All));
        assert_eq!(ids(state.recents()), ["b", "c", "a", "p"]);
        assert_eq!(state.update(Message::TogglePin("c".into())).1, Some(Event::SetPinned { id: "c".into(), pinned: true }));
        state.upsert_session(summary("c", "k-desk", 400, false));
        state.upsert_session(summary("later", "k-laptop", 3, false));
        assert_eq!(ids(state.pinned()), ["c", "x", "later"], "pins survive fresh summaries");
        state.set_projects(["Harness".to_owned(), "Website".to_owned()]);
        for (query, recents, projects) in [("LAPTOP", vec!["b"], vec![]), ("harness refac", vec!["p"], vec![]), ("web", vec![], vec!["Website"])] {
            let _ = state.update(Message::Search(query.into()));
            assert_eq!((ids(state.recents()), state.projects()), (recents, projects), "{query}");
        }
    }

    #[test]
    fn renaming_a_node_updates_search_and_events_follow_messages() {
        let mut state = State::default();
        state.set_sessions([summary("a", "k-new", 1, false)]);
        let _ = state.update(Message::Search("studio".into()));
        assert!(state.recents().is_empty());
        state.set_nodes(Nodes::new(vec![NodeStatus { key: "k-new".into(), name: "Studio".into(), online: true, local: false }]));
        assert_eq!(ids(state.recents()), ["a"]);
        let cases = [
            (Message::OpenSession("a".into()), Event::OpenSession("a".into())),
            (Message::ToggleProjectPin("beta".into()), Event::SetProjectPinned { project: "beta".into(), pinned: true }),
            (Message::OpenUsage, Event::OpenUsage),
            (Message::OpenSettings, Event::OpenSettings),
            (Message::Delete("a".into()), Event::DeleteSession("a".into())),
        ];
        for (m, e) in cases { assert_eq!(state.update(m).1, Some(e)) }
        assert_eq!(state.active(), Some("a"));
    }
}
