//! The left sidebar: machine switcher, New session, Search, Projects (the highlighted one lists its sessions),
//! Pinned, Recents and a footer.

use crate::data::Nodes;
use loa_view::shell::{NodeFilter, NodeStatus, SessionSummary, SidebarIntent, UsageMeter};
use loa_view::theme::Icon;
use iced::widget::{Column, button, column, container, hover, row, rule, scrollable, space, text, text_input, tooltip};
use iced::{Alignment, Element, Fill, Padding, Task};
use loa_ui_kit::icons::tinted;
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{ColorRole as C, RadiusRole, SpaceRole as Sp, TextRole as T, pad_x, sp, text_size as ts, tokens};
use loa_ui_kit::widgets::{label, meter};
use std::collections::{BTreeSet, HashMap};

pub const SEARCH_ID: &str = "loa-sidebar-search";

/// Recents rows drawn at first, and added by each "Show more".
pub const RECENTS_PAGE: usize = 50;

/// Sessions listed under the highlighted project; the rest are on its page.
pub const PROJECT_SESSIONS: usize = 8;

#[derive(Debug, Clone, Default)]
pub struct State {
    /// Sessions with their lowercase search text, rebuilt when sessions or node names change.
    sessions: Vec<(SessionSummary, String)>,
    projects: Vec<String>,
    nodes: Nodes,
    pins: BTreeSet<String>,
    pinned_projects: BTreeSet<String>,
    /// Live local sessions currently running a turn; kept separate from DOM-derived summaries.
    running: BTreeSet<String>,
    query: String,
    /// `query` in lowercase, matched against the haystacks.
    lower_query: String,
    active: Option<String>,
    pub(crate) active_project: Option<String>,
    pub(crate) shown_project: Option<String>,
    updates: usize,
    /// `None` lists this computer.
    machine: Option<NodeFilter>,
    /// The machine whose sessions are listed: `machine` resolved against the nodes.
    filter: NodeFilter,
    /// Indices into `sessions` of the listed pinned and unpinned sessions, newest first. Kept in step by every
    /// change to their inputs, so drawing neither sorts nor lowercases.
    pinned_order: Vec<usize>,
    recent_order: Vec<usize>,
    /// Indices into `projects` of the listed projects, pinned ones first.
    visible_projects: Vec<usize>,
    /// "Show more" pages of Recents opened beyond the first.
    recents_pages: usize,
    machines_open: bool,
    usage: Option<UsageMeter>,
    /// The session whose "Move to project" list is open.
    move_menu: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Emit(SidebarIntent),
    Search(String),
    OpenPeers,
    ToggleMachines,
    PickMachine(NodeFilter),
    OpenSession(String),
    TogglePin(String),
    ToggleProjectPin(String),
    /// The row under Recents that lists the next page of sessions.
    ShowMoreRecents,
    /// The folder button on a session row: opens or closes its project list.
    ToggleMoveMenu(String),
    /// A project picked in a session's list; `None` is "No project".
    MoveTo { id: String, project: Option<String> },
}

impl State {
    pub fn set_sessions(&mut self, sessions: impl IntoIterator<Item = SessionSummary>) {
        self.sessions = sessions.into_iter().map(|s| self.entry(s)).collect();
        self.running.retain(|id| self.sessions.iter().any(|(s, _)| s.id == *id));
        self.reorder();
    }
    pub fn upsert_session(&mut self, summary: SessionSummary) {
        self.put(summary);
        self.reorder();
    }
    /// [`Self::upsert_session`] for many sessions, sorting once.
    pub fn upsert_sessions(&mut self, summaries: impl IntoIterator<Item = SessionSummary>) {
        let mut index: HashMap<String, usize> = self.sessions.iter().enumerate().map(|(at, (s, _))| (s.id.clone(), at)).collect();
        for summary in summaries {
            let entry = self.entry(summary);
            match index.get(&entry.0.id) {
                Some(&at) => self.sessions[at] = entry,
                None => {
                    index.insert(entry.0.id.clone(), self.sessions.len());
                    self.sessions.push(entry);
                }
            }
        }
        self.reorder();
    }
    fn put(&mut self, summary: SessionSummary) {
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
        self.reorder();
    }
    pub fn pins(&self) -> &BTreeSet<String> { &self.pins }
    pub fn remove_session(&mut self, id: &str) {
        self.sessions.retain(|(s, _)| s.id != id);
        self.running.remove(id);
        self.reorder();
    }
    /// Marks whether session `id` is actively running a turn, separate from its DOM summary.
    pub fn set_running(&mut self, id: &str, running: bool) {
        if running {
            self.running.insert(id.to_owned());
        } else {
            self.running.remove(id);
        }
    }
    pub fn is_running(&self, id: &str) -> bool { self.running.contains(id) }
    /// Whether session `id` displays the running indicator; a waiting decision takes precedence.
    pub fn shows_running(&self, id: &str) -> bool { self.session(id).is_some_and(|s| self.row_running(s)) }
    fn row_running(&self, s: &SessionSummary) -> bool { !s.waiting && self.is_running(&s.id) }
    pub fn session(&self, id: &str) -> Option<&SessionSummary> { self.sessions.iter().map(|(s, _)| s).find(|s| s.id == id) }
    pub fn set_pinned_projects(&mut self, pinned: impl IntoIterator<Item = String>) {
        self.pinned_projects = pinned.into_iter().collect();
        self.refilter_projects();
    }
    pub fn pinned_projects(&self) -> &BTreeSet<String> { &self.pinned_projects }
    pub fn set_projects(&mut self, projects: impl IntoIterator<Item = String>) {
        self.projects = projects.into_iter().collect();
        self.refilter_projects();
    }
    pub fn nodes(&self) -> &Nodes { &self.nodes }
    pub fn set_active(&mut self, id: Option<String>) { self.active = id }
    pub fn active(&self) -> Option<&str> { self.active.as_deref() }
    pub fn set_updates(&mut self, count: usize) { self.updates = count }
    pub fn updates(&self) -> usize { self.updates }
    pub fn set_usage(&mut self, usage: Option<UsageMeter>) { self.usage = usage }
    /// Pinned sessions on the listed machine that match the search, most recently updated first.
    pub fn pinned(&self) -> Vec<&SessionSummary> { self.pinned_order.iter().map(|&i| &self.sessions[i].0).collect() }
    /// Unpinned sessions on the listed machine that match the search, most recently updated first; the view draws
    /// the first pages of them.
    pub fn recents(&self) -> Vec<&SessionSummary> { self.recent_order.iter().map(|&i| &self.sessions[i].0).collect() }
    pub fn set_nodes(&mut self, nodes: Vec<NodeStatus>) {
        self.nodes = Nodes::new(nodes);
        let sessions = std::mem::take(&mut self.sessions);
        self.sessions = sessions.into_iter().map(|(s, _)| self.entry(s)).collect();
        self.reorder();
    }
    /// Projects whose name matches the search: pinned ones by name, then the rest in the order given.
    pub fn projects(&self) -> Vec<&str> { self.visible_projects.iter().map(|&i| self.projects[i].as_str()).collect() }
    /// How many Recents rows the view draws.
    fn recents_limit(&self) -> usize { RECENTS_PAGE * (self.recents_pages + 1) }

    /// Resolves the listed machine (the user's pick, else this computer, else every machine) and rebuilds the
    /// Pinned and Recents orders. A different machine starts Recents at one page again.
    fn reorder(&mut self) {
        let filter = self.machine.clone().unwrap_or_else(|| self.nodes.iter().find(|n| n.local).map_or(NodeFilter::All, |n| NodeFilter::Node(n.key.clone())));
        if filter != self.filter {
            (self.filter, self.recents_pages) = (filter, 0);
        }
        let terms: Vec<&str> = self.lower_query.split_whitespace().collect();
        let (mut pinned, mut recent) = (Vec::new(), Vec::new());
        for (i, (s, hay)) in self.sessions.iter().enumerate() {
            if self.filter.matches(&s.node) && terms.iter().all(|t| hay.contains(t)) {
                if s.pinned { pinned.push(i) } else { recent.push(i) }
            }
        }
        let newest = |a: &usize, b: &usize| {
            let (a, b) = (&self.sessions[*a].0, &self.sessions[*b].0);
            b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id))
        };
        pinned.sort_by(newest);
        recent.sort_by(newest);
        (self.pinned_order, self.recent_order) = (pinned, recent);
    }

    /// Rebuilds the listed projects from the names, pins and search.
    fn refilter_projects(&mut self) {
        let terms: Vec<&str> = self.lower_query.split_whitespace().collect();
        let pinned = |i: &usize| self.pinned_projects.contains(&self.projects[*i]);
        let mut first: Vec<usize> = (0..self.projects.len()).filter(pinned).collect();
        first.sort_by(|a, b| self.projects[*a].cmp(&self.projects[*b]));
        let rest = (0..self.projects.len()).filter(|i| !pinned(i));
        let visible = first.into_iter().chain(rest).filter(|i| {
            let name = self.projects[*i].to_lowercase();
            terms.iter().all(|t| name.contains(t))
        });
        self.visible_projects = visible.collect();
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<SidebarIntent>) {
        let event = match message {
            Message::Emit(event) => Some(event),
            Message::Search(query) => {
                (self.lower_query, self.query) = (query.to_lowercase(), query);
                self.reorder();
                self.refilter_projects();
                None
            }
            Message::ToggleMachines => {
                self.machines_open = !self.machines_open;
                None
            }
            Message::PickMachine(filter) => {
                (self.machine, self.machines_open) = (Some(filter), false);
                self.reorder();
                None
            }
            Message::OpenPeers => {
                self.machines_open = false;
                Some(SidebarIntent::OpenPeers)
            }
            Message::OpenSession(id) => {
                self.active = Some(id.clone());
                Some(SidebarIntent::OpenSession(id))
            }
            Message::ToggleProjectPin(project) => {
                let pinned = self.pinned_projects.insert(project.clone()) || !self.pinned_projects.remove(&project);
                self.refilter_projects();
                Some(SidebarIntent::SetProjectPinned { pinned, project })
            }
            Message::TogglePin(id) => {
                let event = self.sessions.iter_mut().find(|(s, _)| s.id == id).map(|(s, _)| {
                    s.pinned = !s.pinned;
                    if s.pinned { self.pins.insert(id.clone()); } else { self.pins.remove(&id); }
                    SidebarIntent::SetPinned { id, pinned: s.pinned }
                });
                self.reorder();
                event
            }
            Message::ShowMoreRecents => {
                self.recents_pages += 1;
                None
            }
            Message::ToggleMoveMenu(id) => {
                self.move_menu = (self.move_menu.as_deref() != Some(id.as_str())).then_some(id);
                None
            }
            Message::MoveTo { id, project } => {
                self.move_menu = None;
                Some(SidebarIntent::MoveSession { id, project })
            }
        };
        (Task::none(), event)
    }

    /// The sidebar in the app's message type. The highlighted project (the one whose page is open, else the active
    /// one, else the active session's) lists its sessions under its row.
    pub fn view_with<'a, M: 'a>(&'a self, map: impl Fn(Message) -> M + Clone + 'a) -> Element<'a, M> {
        let plus = container(tinted(Icon::Plus, T::Label, Tone::Normal)).center(sp(Sp::Xl)).style(style::fill(C::Accent, tokens().radius(RadiusRole::Pill)));
        let new = row![plus, label("New session", T::Body, Tone::Normal).width(Fill), label("Ctrl N", T::Caption, Tone::Faint)];
        let icon = text_input::Icon { font: iced::Font::default(), code_point: '⌕', size: Some(ts(T::Body).into()), spacing: sp(Sp::S), side: text_input::Side::Left };
        let search = text_input("Search", &self.query).id(SEARCH_ID).on_input(Message::Search).icon(icon).size(ts(T::Body)).style(style::input).padding(row_pad());
        let top = column![self.machine_switcher(), row_button(line(new), false, Message::Emit(SidebarIntent::NewSession)), search].spacing(sp(Sp::Xs));
        // "Projects" where the other headers sit, with the `+` beside it, lined up with the row actions below.
        let add = row_action(Icon::Plus, Message::Emit(SidebarIntent::NewProject));
        let projects_header = row![label("Projects", T::Caption, Tone::Faint).width(Fill), add].align_y(Alignment::Center);
        let projects_header = container(projects_header).padding(Padding { top: sp(Sp::M), right: sp(Sp::Xs), bottom: 0.0, left: row_pad().left });
        let mut lists = Column::new().push(self.machines_open.then(|| self.machine_list())).push(projects_header);
        let active_session = self.active.as_deref().and_then(|id| self.session(id));
        let active_project = self.shown_project.as_deref().or(self.active_project.as_deref()).or_else(|| active_session.and_then(|s| s.project.as_deref()));
        if self.visible_projects.is_empty() { lists = lists.push(hint(if self.projects.is_empty() { "No projects yet" } else { "No match" })) }
        for &index in &self.visible_projects {
            let project = self.projects[index].as_str();
            let on = active_project == Some(project);
            let pin = self.pinned_projects.contains(project).then(|| tinted(Icon::Pin, T::Caption, Tone::Muted));
            let body = row_button(line(row![tinted(Icon::Folder, T::Body, Tone::Muted), label(project, T::Body, Tone::Normal).width(Fill), pin]), on, Message::Emit(SidebarIntent::OpenProject(project.to_owned())));
            let actions = [
                row_action(Icon::Plus, Message::Emit(SidebarIntent::NewSessionIn(project.to_owned()))),
                row_action(Icon::Pin, Message::ToggleProjectPin(project.to_owned())),
                row_action(Icon::X, Message::Emit(SidebarIntent::DeleteProject(project.to_owned()))),
            ];
            lists = lists.push(hover(body, row_actions(actions, on)));
            if on {
                lists = self.project_sessions(lists, project);
            }
        }
        if !self.pinned_order.is_empty() {
            lists = lists.push(header("Pinned")).extend(self.pinned_order.iter().map(|&i| self.session_row(&self.sessions[i].0)));
        }
        lists = lists.push(header("Recents"));
        if self.recent_order.is_empty() { lists = lists.push(hint(if self.query.is_empty() { "No sessions on this machine" } else { "No match" })) }
        let limit = self.recents_limit();
        lists = lists.extend(self.recent_order.iter().take(limit).map(|&i| self.session_row(&self.sessions[i].0)));
        let hidden = self.recent_order.len().saturating_sub(limit);
        if hidden > 0 {
            lists = lists.push(row_button(label(format!("Show {} more", hidden.min(RECENTS_PAGE)), T::Label, Tone::Muted), false, Message::ShowMoreRecents));
        }
        // Scrolls by wheel and trackpad with no bar, which would sit over the rows' ends and their hover actions.
        let hidden_bar = scrollable::Direction::Vertical(scrollable::Scrollbar::hidden());
        let list = Element::from(scrollable(lists.spacing(sp(Sp::Xs) / 2.0)).direction(hidden_bar).height(Fill)).map(map.clone());
        let content = column![Element::from(top).map(map.clone()), list, Element::from(self.footer()).map(map)];
        container(content.spacing(sp(Sp::Xs)).padding(sp(Sp::S))).style(style::container(Surface::Side)).width(Fill).height(Fill).into()
    }

    /// Pushes `project`'s sessions under its row, indented, pinned first then newest, at most [`PROJECT_SESSIONS`]
    /// with a row to the project's page for the rest.
    fn project_sessions<'a>(&'a self, mut lists: Column<'a, Message>, project: &'a str) -> Column<'a, Message> {
        let mut sessions = self.pinned_order.iter().chain(&self.recent_order).map(|&i| &self.sessions[i].0).filter(|s| s.project.as_deref() == Some(project));
        let indent = |e: Element<'a, Message>| container(e).padding(Padding::ZERO.left(sp(Sp::M)));
        let mut shown = 0;
        for session in sessions.by_ref().take(PROJECT_SESSIONS) {
            lists = lists.push(indent(self.session_row(session)));
            shown += 1;
        }
        if shown == 0 {
            return lists.push(indent(hint("No sessions yet")));
        }
        let more = sessions.count();
        if more > 0 {
            let all = row_button(label(format!("All {} sessions", shown + more), T::Label, Tone::Muted), false, Message::Emit(SidebarIntent::OpenProject(project.to_owned())));
            lists = lists.push(indent(all));
        }
        lists
    }

    /// Name, sub line and whether the dot shows online, for a machine choice.
    fn describe(&self, filter: &NodeFilter) -> (String, String, bool) {
        let node_label = |n: &NodeStatus| if n.local { "This computer" } else if n.online { "Peer · online" } else { "Peer · offline" };
        match filter {
            NodeFilter::All => ("All machines".into(), "Recents from every machine".into(), true),
            NodeFilter::Node(key) => match self.nodes.get(key) {
                Some(n) => (n.name.clone(), node_label(n).into(), n.local || n.online),
                None => (self.nodes.name_of(key).into(), "Unknown machine".into(), false),
            },
        }
    }
    fn machine_switcher(&self) -> Element<'_, Message> {
        let (name, sub, online) = self.describe(&self.filter);
        let bold = iced::Font { weight: iced::font::Weight::Bold, ..tokens().font(T::Body) };
        let chevron = tinted(if self.machines_open { Icon::ChevronUp } else { Icon::ChevronDown }, T::Body, Tone::Muted);
        let names = column![label(name, T::Body, Tone::Normal).font(bold), label(sub, T::Caption, Tone::Muted)].width(Fill);
        row_button(line(row![status_dot(online), names, chevron]), self.machines_open, Message::ToggleMachines)
    }
    fn machine_list(&self) -> Element<'_, Message> {
        let filters = std::iter::once(NodeFilter::All).chain(self.nodes.iter().map(|n| NodeFilter::Node(n.key.clone())));
        let rows = filters.map(|filter| {
            let (name, sub, online) = self.describe(&filter);
            let (count, waiting) = self.sessions.iter().filter(|(s, _)| filter.matches(&s.node)).fold((0, 0), |(n, w), (s, _)| (n + 1, w + usize::from(s.waiting)));
            let tally = if waiting > 0 { label(format!("{waiting} need you"), T::Caption, Tone::Warn) } else { label(format!("{count} sessions"), T::Caption, Tone::Faint) };
            let names = column![label(name, T::Body, Tone::Normal), label(sub, T::Caption, Tone::Faint)].width(Fill);
            row_button(line(row![status_dot(online), names, tally]), filter == self.filter, Message::PickMachine(filter))
        });
        let peers = button(label("Manage peers", T::Label, Tone::Normal)).on_press(Message::OpenPeers).style(style::button(ButtonKind::Ghost)).padding(row_pad());
        container(Column::with_children(rows).push(peers).spacing(sp(Sp::Xs) / 2.0)).style(style::container(Surface::Popup)).padding(sp(Sp::Xs)).into()
    }
    fn footer(&self) -> Element<'_, Message> {
        let updates = (self.updates > 0).then(|| footer_item(Icon::ArrowUp, "Updates", Some(label(self.updates.to_string(), T::Caption, Tone::Warn).into()), SidebarIntent::OpenUpdates));
        let meter = self.usage.as_ref().map(|u| {
            let f = u.fraction.clamp(0.0, 1.0);
            line(row![container(meter(f)).width(sp(Sp::Xl) * 2.0), label(format!("{} {:.0}%", u.label, f * 100.0), T::Caption, Tone::Faint)]).into()
        });
        let items = column![updates, footer_item(Icon::Gauge, "Usage", meter, SidebarIntent::OpenUsage), footer_item(Icon::SlidersHorizontal, "Configs", None, SidebarIntent::OpenConfigs), footer_item(Icon::Settings, "Settings", None, SidebarIntent::OpenSettings)];
        column![rule::horizontal(1).style(style::rule), items.spacing(sp(Sp::Xs) / 2.0)].spacing(sp(Sp::Xs)).into()
    }
    /// A session row with its hover actions (move to a project, pin, delete) and, while open, its project list:
    /// "No project", then every project.
    fn session_row<'a>(&'a self, s: &'a SessionSummary) -> Element<'a, Message> {
        let selected = self.shown_project.is_none() && self.active.as_deref() == Some(s.id.as_str());
        let title = container(label(s.title.as_str(), T::Body, Tone::Normal).wrapping(text::Wrapping::None)).width(Fill).clip(true);
        let node = (self.filter == NodeFilter::All).then(|| label(self.nodes.name_of(&s.node), T::Caption, Tone::Faint));
        let actions = [
            row_action(Icon::Folder, Message::ToggleMoveMenu(s.id.clone())),
            row_action(Icon::Pin, Message::TogglePin(s.id.clone())),
            row_action(Icon::X, Message::Emit(SidebarIntent::DeleteSession(s.id.clone()))),
        ];
        let status: Option<Element<'a, Message>> = if s.waiting {
            Some(label("Needs you", T::Caption, Tone::Warn).into())
        } else if self.row_running(s) {
            Some(running_indicator())
        } else {
            None
        };
        let session = hover(row_button(line(row![title, status, node]), selected, Message::OpenSession(s.id.clone())), row_actions(actions, selected));
        if self.move_menu.as_deref() != Some(s.id.as_str()) {
            return session;
        }
        let choice = |name: &'a str, project: Option<&'a String>| {
            let current = s.project.as_ref() == project;
            let check = current.then(|| tinted(Icon::Check, T::Caption, Tone::Muted));
            row_button(line(row![label(name, T::Label, Tone::Normal).width(Fill), check]), current, Message::MoveTo { id: s.id.clone(), project: project.cloned() })
        };
        let choices = std::iter::once(choice("No project", None)).chain(self.projects.iter().map(|p| choice(p, Some(p))));
        let menu = container(Column::with_children(choices).spacing(sp(Sp::Xs) / 4.0)).padding(Padding::ZERO.left(sp(Sp::M)).bottom(sp(Sp::Xs)));
        column![session, menu].into()
    }
}

fn row_pad() -> Padding { Padding::from([sp(Sp::Xs) * 1.75, sp(Sp::S) * 1.25]) }
fn line<'a, M>(r: iced::widget::Row<'a, M>) -> iced::widget::Row<'a, M> { r.spacing(sp(Sp::S)).align_y(Alignment::Center) }
fn row_button<'a>(content: impl Into<Element<'a, Message>>, selected: bool, on: Message) -> Element<'a, Message> {
    button(content).on_press(on).style(style::button(ButtonKind::Row { selected })).padding(row_pad()).width(Fill).into()
}
fn status_dot<'a>(online: bool) -> Element<'a, Message> {
    container(space()).width(sp(Sp::S)).height(sp(Sp::S)).style(style::fill(if online { C::Ok } else { C::Text3 }, tokens().radius(RadiusRole::Dot))).into()
}
fn row_action<'a>(glyph: Icon, on: Message) -> Element<'a, Message> {
    button(tinted(glyph, T::Label, Tone::Muted)).on_press(on).style(style::button(ButtonKind::Ghost)).padding(sp(Sp::Xs)).into()
}

/// Hover actions on a backing the colour of the row, so they cover the end of the title.
fn row_actions<'a>(actions: impl IntoIterator<Item = Element<'a, Message>>, selected: bool) -> Element<'a, Message> {
    let half = sp(Sp::Xs) / 2.0;
    let actions = container(row(actions).spacing(half)).padding(Padding::ZERO.left(half).right(half)).style(style::fill(if selected { C::Selected } else { C::Hover }, 0.0));
    container(actions).align_right(Fill).center_y(Fill).padding(pad_x(Sp::Xs)).into()
}
fn footer_item<'a>(glyph: Icon, text: &'a str, trailing: Option<Element<'a, Message>>, on: SidebarIntent) -> Element<'a, Message> {
    row_button(line(row![tinted(glyph, T::Body, Tone::Muted), label(text, T::Body, Tone::Normal).width(Fill), trailing]), false, Message::Emit(on))
}

fn header(text: &str) -> Element<'_, Message> { container(label(text, T::Caption, Tone::Faint)).padding(Padding::from([sp(Sp::Xs), row_pad().left]).top(sp(Sp::M))).into() }
fn hint(text: &str) -> Element<'_, Message> { container(label(text, T::Label, Tone::Faint)).padding(Padding::from([sp(Sp::Xs), row_pad().left])).into() }
fn running_indicator<'a>() -> Element<'a, Message> {
    let icon = tinted(Icon::LoaderCircle, T::Caption, Tone::Accent);
    let tip = container(label("Running", T::Caption, Tone::Normal))
        .padding([sp(Sp::Xs), sp(Sp::S)])
        .style(style::container(Surface::Tooltip));
    tooltip(icon, tip, tooltip::Position::Bottom).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, node: &str, updated: i64, pinned: bool) -> SessionSummary {
        SessionSummary { id: id.into(), title: format!("Title {id}"), project: (id == "p").then(|| "Harness".into()), node: node.into(), pinned, waiting: false, updated }
    }

    fn ids(list: Vec<&SessionSummary>) -> Vec<&str> { list.into_iter().map(|s| s.id.as_str()).collect() }

    fn node(key: &str, name: &str, local: bool) -> NodeStatus { NodeStatus { key: key.into(), name: name.into(), online: local, local } }

    #[test]
    fn machine_filter_pins_and_search_shape_the_lists() {
        let mut state = State::default();
        state.set_sessions([summary("a", "k-desk", 100, false), summary("b", "k-laptop", 300, false), summary("c", "k-desk", 200, false), summary("x", "k-desk", 10, true), summary("p", "k-desk", 5, false)]);
        state.set_pins(["later".to_owned(), "x".to_owned()]);
        state.set_nodes(vec![node("k-desk", "desk", true), node("k-laptop", "laptop", false)]);
        assert_eq!((ids(state.recents()), ids(state.pinned())), (vec!["c", "a", "p"], vec!["x"]));
        let _ = state.update(Message::PickMachine(NodeFilter::All));
        assert_eq!(ids(state.recents()), ["b", "c", "a", "p"]);
        assert_eq!(state.update(Message::TogglePin("c".into())).1, Some(SidebarIntent::SetPinned { id: "c".into(), pinned: true }));
        state.upsert_session(summary("c", "k-desk", 400, false));
        state.upsert_session(summary("later", "k-laptop", 3, false));
        assert_eq!(ids(state.pinned()), ["c", "x", "later"], "pins survive fresh summaries");
        state.set_projects(["Harness".to_owned(), "Website".to_owned()]);
        for (query, recents, projects) in [("LAPTOP", vec!["b"], vec![]), ("harness title p", vec!["p"], vec![]), ("web", vec![], vec!["Website"])] {
            let _ = state.update(Message::Search(query.into()));
            assert_eq!((ids(state.recents()), state.projects()), (recents, projects), "{query}");
        }
        assert_eq!(state.update(Message::OpenSession("a".into())).1, Some(SidebarIntent::OpenSession("a".into())));
        assert_eq!(state.update(Message::ToggleProjectPin("beta".into())).1, Some(SidebarIntent::SetProjectPinned { project: "beta".into(), pinned: true }));
        assert_eq!(state.active(), Some("a"));
    }

    #[test]
    fn recents_show_a_page_at_a_time_and_a_new_machine_starts_over() {
        let mut state = State::default();
        state.set_nodes(vec![node("k-desk", "desk", true), node("k-laptop", "laptop", false)]);
        state.set_sessions((0..120).map(|n| SessionSummary { title: "Session".into(), ..summary(&format!("s{n}"), "k-desk", n, false) }));
        assert_eq!(state.recents().len(), 120, "every match stays listed for navigation");
        assert_eq!(state.recents_limit(), RECENTS_PAGE);

        let _ = state.update(Message::ShowMoreRecents);
        assert_eq!(state.recents_limit(), 2 * RECENTS_PAGE);
        // A new summary keeps the pages the user opened.
        state.upsert_session(SessionSummary { title: "Renamed".into(), ..summary("s0", "k-desk", 500, false) });
        assert_eq!(state.recents_limit(), 2 * RECENTS_PAGE);
        assert_eq!(ids(state.recents())[0], "s0", "the order follows the update");

        let _ = state.update(Message::PickMachine(NodeFilter::All));
        assert_eq!(state.recents_limit(), RECENTS_PAGE);
    }

    #[test]
    fn a_session_moves_to_a_project_or_out_of_every_project() {
        let mut state = State::default();
        state.set_projects(["alpha".to_owned(), "beta".to_owned()]);
        state.set_sessions([summary("a", "k", 1, false)]);
        let _ = state.update(Message::ToggleMoveMenu("a".into()));
        assert_eq!(state.move_menu.as_deref(), Some("a"));
        let _ = state.view_with(|m| m);
        let moved = state.update(Message::MoveTo { id: "a".into(), project: Some("beta".into()) }).1;
        assert_eq!(moved, Some(SidebarIntent::MoveSession { id: "a".into(), project: Some("beta".into()) }));
        assert!(state.move_menu.is_none(), "picking closes the list");
        let _ = state.update(Message::ToggleMoveMenu("a".into()));
        let _ = state.update(Message::ToggleMoveMenu("a".into()));
        assert!(state.move_menu.is_none(), "the folder button closes it again");
        let out = state.update(Message::MoveTo { id: "a".into(), project: None }).1;
        assert_eq!(out, Some(SidebarIntent::MoveSession { id: "a".into(), project: None }));
    }

    #[test]
    fn the_active_project_lists_its_sessions_under_its_row() {
        fn sim(state: &State) -> iced_test::Simulator<'_, Message> { iced_test::Simulator::with_size(iced::Settings::default(), iced::Size::new(400.0, 2000.0), state.view_with(|m| m)) }
        let harness = |id: String, updated| SessionSummary { project: Some("harness".into()), title: format!("Harness task {id}"), ..summary(&id, "k-desk", updated, false) };
        let mut state = State::default();
        state.set_nodes(vec![node("k-desk", "desk", true)]);
        state.set_projects(["harness".to_owned(), "other".to_owned()]);
        state.set_sessions([harness("s1".into(), 30)]);

        state.active_project = Some("other".into());
        assert!(sim(&state).find("No sessions yet").is_ok(), "an empty project says so under its row");

        // More sessions than fit link to the project's page.
        state.upsert_sessions((0..9).map(|i| harness(format!("h{i}"), 100 + i)));
        state.active_project = Some("harness".into());
        let mut ui = sim(&state);
        assert!(ui.find("No sessions yet").is_err());
        ui.click("All 10 sessions").expect("a row to the rest of the project's sessions");
        let events: Vec<_> = ui.into_messages().filter_map(|m| state.update(m).1).collect();
        assert_eq!(events, [SidebarIntent::OpenProject("harness".into())]);

        state.active_project = None;
        assert!(sim(&state).find("No sessions yet").is_err(), "no highlighted project, no list");
    }

    #[test]
    fn running_state_survives_summaries_yields_to_waiting_and_clears_on_removal() {
        fn sim(state: &State) -> iced_test::Simulator<'_, Message> { iced_test::Simulator::with_size(iced::Settings::default(), iced::Size::new(400.0, 2000.0), state.view_with(|m| m)) }
        let mut state = State::default();
        state.set_nodes(vec![node("k-desk", "desk", true)]);
        state.set_sessions([summary("a", "k-desk", 10, false), summary("b", "k-desk", 20, false)]);
        assert!(!state.is_running("a") && !state.shows_running("a"));

        state.set_running("a", true);
        assert!(state.is_running("a") && state.shows_running("a"));
        assert!(!state.is_running("b") && !state.shows_running("b"));

        // Fresh DOM summaries and node refreshes keep the separate running state intact.
        state.upsert_session(SessionSummary { title: "Updated a".into(), ..summary("a", "k-desk", 30, false) });
        state.set_nodes(vec![node("k-desk", "desk", true)]);
        assert!(state.is_running("a") && state.shows_running("a"), "running survives summary and node updates");
        assert!(sim(&state).find("Updated a").is_ok() && sim(&state).find("Needs you").is_err());

        // Waiting takes precedence over running, then running resumes once waiting clears.
        state.upsert_session(SessionSummary { title: "Updated a".into(), waiting: true, ..summary("a", "k-desk", 35, false) });
        assert!(state.is_running("a") && !state.shows_running("a"), "waiting wins over running");
        assert!(sim(&state).find("Needs you").is_ok());

        state.upsert_session(SessionSummary { title: "Updated a".into(), waiting: false, ..summary("a", "k-desk", 40, false) });
        assert!(state.is_running("a") && state.shows_running("a"), "running shows again once waiting clears");
        assert!(sim(&state).find("Needs you").is_err());

        // Clearing via set_running(false), remove_session, and set_sessions drops stale flags.
        state.set_running("b", true);
        state.set_running("a", false);
        assert!(!state.is_running("a") && !state.shows_running("a"));

        state.remove_session("b");
        assert!(!state.is_running("b") && !state.shows_running("b"), "remove_session clears running state");
        state.upsert_session(summary("b", "k-desk", 50, false));
        assert!(!state.is_running("b") && !state.shows_running("b"), "re-added session stays idle");

        state.set_running("a", true);
        state.set_running("b", true);
        state.set_sessions([summary("b", "k-desk", 50, false)]);
        assert!(!state.is_running("a") && state.is_running("b"), "set_sessions drops running state for removed sessions");
    }
}
