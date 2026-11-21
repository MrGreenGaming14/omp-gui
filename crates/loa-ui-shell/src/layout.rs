//! The app frame: the sidebar, the top bar above a center slot, and a toggleable right panel
//! with a tab bar. The sidebar and panel resize by their edges, each pane can fill the window,
//! and the panel keeps its width and tab while closed.

use crate::data::{NodeStatus, Nodes, SessionSummary, UsageMeter, short_key};
use crate::theme::{self, Appearance, ThemeRef};
use crate::{sidebar, topbar};
use iced::theme::Mode;
use iced::widget::operation::{self, AbsoluteOffset, RelativeOffset};
use iced::widget::{button, column, container, mouse_area, row, rule, scrollable, sensor};
use iced::{Alignment, Element, Fill, Length, Size, Subscription, Task, Theme, mouse};
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, pad_x, sp};
use loa_ui_kit::widgets::{Tab, icon_button, label, splitter_edge, splitter_width, tab_strip};

pub const SIDEBAR_WIDTH: f32 = 272.0;
pub const MIN_SIDEBAR_WIDTH: f32 = 200.0;
pub const MAX_SIDEBAR_WIDTH: f32 = 480.0;
pub const MIN_PANEL_WIDTH: f32 = 240.0;
pub const DEFAULT_PANEL_WIDTH: f32 = 420.0;
/// Narrowest the center gets while the panel is resized.
pub const MIN_CENTER_WIDTH: f32 = 320.0;
/// Ids of the panel and project tab strips, which scroll sideways when they overflow.
pub const PANEL_TABS_ID: &str = "loa-panel-tabs";
pub const PROJECT_TABS_ID: &str = "loa-project-tabs";
/// Container id of the active tab's pop-out button.
pub const PANEL_DETACH_ID: &str = "loa-panel-detach";
const PROJECT_TABS_HEIGHT: f32 = 34.0;
/// Pixels a mouse wheel line scrolls a tab strip.
const WHEEL_LINE: f32 = 40.0;

/// A pane that can fill the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Sidebar,
    Center,
    Panel,
}

/// One tab of the right panel; the app draws the body of the active one by `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelTab {
    pub id: String,
    pub title: String,
    pub icon: Option<Icon>,
}

impl PanelTab {
    pub fn new(id: impl Into<String>, title: impl Into<String>, icon: Option<Icon>) -> Self { Self { id: id.into(), title: title.into(), icon } }
}

#[derive(Debug, Clone)]
pub struct State {
    sidebar: sidebar::State,
    topbar: topbar::State,
    theme: theme::State,
    panel_open: bool,
    panel_width: f32,
    tabs: Vec<PanelTab>,
    /// The tab the user picked; may name a tab not added yet, then the first tab shows.
    active: Option<String>,
    frame: Option<Size>,
    project_tabs: bool,
    open_projects: Vec<String>,
    active_project: Option<String>,
    sidebar_width: f32,
    /// The edge held: the sidebar's (`Pane::Sidebar`) or the panel's.
    dragging: Option<Pane>,
    full: Option<Pane>,
    /// Tabs shown in their own window instead of the panel.
    detached: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Sidebar(sidebar::Message),
    SelectTab(String),
    TabsWheel(mouse::ScrollDelta),
    OpenProjectTab(String),
    CloseProjectTab(String),
    ProjectTabsWheel(mouse::ScrollDelta),
    /// The panel edge was grabbed.
    DragStart,
    SidebarDragStart,
    /// The cursor moved to this x while an edge is held.
    DragMove(f32),
    DragEnd,
    TogglePanel,
    ToggleFull(Pane),
    /// Pop the tab out into its own window.
    Detach(String),
    ShowDetached(String),
    Reattach(String),
    FrameSized(Size),
    SetTheme(ThemeRef),
    SystemTheme(Mode),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Navigation from the sidebar, the top bar crumb or the project tabs.
    Sidebar(sidebar::Event),
    /// Drop this project's tab; its sessions stay open.
    CloseProjectTab(String),
    DetachTab(String),
    ShowDetached(String),
    ReattachTab(String),
    /// The user picked a theme; persist it.
    SetTheme(ThemeRef),
    /// The active tokens changed to this generation; rebuild cached looks.
    ThemeChanged(u64),
}

impl State {
    /// The frame with the panel open; the task asks the system for its theme.
    pub fn new(appearance: Appearance) -> (Self, Task<Message>) {
        let topbar = topbar::State { panel_open: true, ..Default::default() };
        let state = Self {
            sidebar: sidebar::State::default(), topbar, theme: theme::State::new(appearance), panel_open: true, panel_width: DEFAULT_PANEL_WIDTH, tabs: Vec::new(), active: None,
            frame: None, project_tabs: false, open_projects: Vec::new(), active_project: None, sidebar_width: SIDEBAR_WIDTH, dragging: None, full: None, detached: Vec::new(),
        };
        (state, theme::State::detect().map(Message::SystemTheme))
    }

    pub fn subscription(&self) -> Subscription<Message> { theme::State::subscription().map(Message::SystemTheme) }

    pub fn theme(&self) -> Theme { self.theme.theme() }

    pub fn theme_state(&self) -> &theme::State { &self.theme }

    pub fn sidebar(&self) -> &sidebar::State { &self.sidebar }

    pub fn topbar(&self) -> &topbar::State { &self.topbar }

    pub fn set_sessions(&mut self, sessions: impl IntoIterator<Item = SessionSummary>) {
        self.sidebar.set_sessions(sessions);
        self.refresh_breadcrumb();
    }

    pub fn upsert_session(&mut self, summary: SessionSummary) {
        self.sidebar.upsert_session(summary);
        self.refresh_breadcrumb();
    }

    pub fn remove_session(&mut self, id: &str) {
        self.sidebar.remove_session(id);
        self.refresh_breadcrumb();
    }

    pub fn set_pins(&mut self, pins: impl IntoIterator<Item = String>) { self.sidebar.set_pins(pins) }

    pub fn set_pinned_projects(&mut self, pinned: impl IntoIterator<Item = String>) { self.sidebar.set_pinned_projects(pinned) }

    /// Highlights the selected project in the sidebar and the project tab strip.
    pub fn set_active_project(&mut self, project: Option<String>) {
        self.sidebar.set_active_project(project.clone());
        self.active_project = project;
    }

    pub fn set_project_tabs(&mut self, enabled: bool) { self.project_tabs = enabled }

    pub fn project_tabs(&self) -> bool { self.project_tabs }

    pub fn set_open_projects(&mut self, projects: Vec<String>) { self.open_projects = projects }

    pub fn open_projects(&self) -> &[String] { &self.open_projects }

    pub fn set_projects(&mut self, projects: impl IntoIterator<Item = String>) { self.sidebar.set_projects(projects) }

    pub fn set_nodes(&mut self, nodes: Vec<NodeStatus>) {
        self.sidebar.set_nodes(Nodes::new(nodes));
        self.refresh_breadcrumb();
    }

    pub fn set_updates(&mut self, count: usize) { self.sidebar.set_updates(count) }

    pub fn updates(&self) -> usize { self.sidebar.updates() }

    pub fn set_usage(&mut self, usage: Option<UsageMeter>) { self.sidebar.set_usage(usage) }

    pub fn full(&self) -> Option<Pane> { self.full }

    pub fn set_detached(&mut self, tabs: Vec<String>) { self.detached = tabs }

    pub fn is_detached(&self, tab: &str) -> bool { self.detached.iter().any(|d| d == tab) }

    pub fn set_active_session(&mut self, id: Option<String>) {
        self.sidebar.set_active(id);
        self.refresh_breadcrumb();
    }

    /// Marks the project whose page fills the center, for the sidebar and the top bar.
    pub fn set_shown_project(&mut self, project: Option<String>) {
        self.sidebar.set_shown_project(project.clone());
        self.topbar.project_page = project;
    }

    /// The short status of the shown session in the top bar, e.g. "Running".
    pub fn set_session_status(&mut self, status: Option<String>) { self.topbar.status = status }

    fn refresh_breadcrumb(&mut self) {
        let sidebar = &self.sidebar;
        self.topbar.crumb = sidebar.active().and_then(|id| sidebar.session(id)).map(|s| {
            let node = sidebar.nodes().get(&s.node).cloned().unwrap_or_else(|| NodeStatus { key: s.node.clone(), name: short_key(&s.node).to_owned(), online: false, local: false });
            topbar::Breadcrumb { project: s.project.clone(), title: s.title.clone(), waiting: s.waiting, node }
        });
    }

    /// Replaces the panel tabs; the active tab stays when it is still there.
    pub fn set_tabs(&mut self, tabs: Vec<PanelTab>) { self.tabs = tabs }

    /// Adds a tab, or replaces the tab with the same id.
    pub fn add_tab(&mut self, tab: PanelTab) {
        match self.tabs.iter_mut().find(|t| t.id == tab.id) {
            Some(t) => *t = tab,
            None => self.tabs.push(tab),
        }
    }

    pub fn tabs(&self) -> &[PanelTab] { &self.tabs }

    /// Opens the panel on this tab; pair it with [`State::reveal_active_tab`].
    pub fn show_tab(&mut self, id: &str) {
        if self.tabs.iter().any(|t| t.id == id) {
            self.active = Some(id.to_owned());
            self.set_panel_open(true);
        }
    }

    /// Snaps an overflowing tab bar to the active tab's share of it, so the first and last
    /// tabs land exactly at its ends.
    pub fn reveal_active_tab<M: Send + 'static>(&self) -> Task<M> {
        let index = self.active_tab().and_then(|a| self.tabs.iter().position(|t| t.id == a)).unwrap_or(0);
        let x = if self.tabs.len() < 2 { 0.0 } else { index as f32 / (self.tabs.len() - 1) as f32 };
        operation::snap_to(PANEL_TABS_ID, RelativeOffset { x: Some(x), y: None })
    }

    /// The id of the tab whose body the panel shows.
    pub fn active_tab(&self) -> Option<&str> {
        self.active.as_deref().filter(|a| self.tabs.iter().any(|t| t.id == *a)).or_else(|| self.tabs.first().map(|t| t.id.as_str()))
    }

    pub fn panel_open(&self) -> bool { self.panel_open }

    /// Opens or closes the panel, keeping width and tab; closing leaves its full screen.
    pub fn set_panel_open(&mut self, open: bool) {
        (self.panel_open, self.topbar.panel_open) = (open, open);
        if self.dragging == Some(Pane::Panel) { self.dragging = None }
        if !open && self.full == Some(Pane::Panel) { self.set_full(None) }
    }

    fn max_panel_width(&self) -> f32 {
        self.frame.map_or(f32::INFINITY, |f| (f.width - self.sidebar_width - MIN_CENTER_WIDTH - 2.0 * splitter_width()).max(MIN_PANEL_WIDTH))
    }

    fn set_full(&mut self, pane: Option<Pane>) {
        self.full = pane;
        self.topbar.full = pane == Some(Pane::Center);
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::Sidebar(m) => {
                let (task, event) = self.sidebar.update(m);
                if matches!(event, Some(sidebar::Event::OpenSession(_))) { self.refresh_breadcrumb() }
                return (task.map(Message::Sidebar), event.map(Event::Sidebar));
            }
            Message::SelectTab(id) => {
                if self.tabs.iter().any(|t| t.id == id) { self.active = Some(id) }
                None
            }
            Message::TabsWheel(delta) => return (wheel_tabs(PANEL_TABS_ID, delta), None),
            Message::ProjectTabsWheel(delta) => return (wheel_tabs(PROJECT_TABS_ID, delta), None),
            Message::OpenProjectTab(p) => Some(Event::Sidebar(sidebar::Event::OpenProject(p))),
            Message::CloseProjectTab(p) => Some(Event::CloseProjectTab(p)),
            Message::DragStart => {
                self.dragging = self.panel_open.then_some(Pane::Panel);
                None
            }
            Message::SidebarDragStart => {
                self.dragging = Some(Pane::Sidebar);
                None
            }
            Message::DragMove(x) => {
                match (self.dragging, self.frame) {
                    (Some(Pane::Panel), Some(f)) => self.panel_width = (f.width - x - splitter_width() / 2.0).min(self.max_panel_width()).max(MIN_PANEL_WIDTH),
                    (Some(Pane::Sidebar), _) => self.sidebar_width = (x - splitter_width() / 2.0).clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH),
                    _ => {}
                }
                None
            }
            Message::DragEnd => {
                self.dragging = None;
                None
            }
            Message::TogglePanel => {
                self.set_panel_open(!self.panel_open);
                None
            }
            Message::ToggleFull(pane) => {
                self.set_full((self.full != Some(pane)).then_some(pane));
                None
            }
            Message::Detach(tab) => Some(Event::DetachTab(tab)),
            Message::ShowDetached(tab) => Some(Event::ShowDetached(tab)),
            Message::Reattach(tab) => Some(Event::ReattachTab(tab)),
            Message::FrameSized(size) => {
                self.frame = Some(size);
                None
            }
            Message::SetTheme(theme) => {
                self.theme.set_preference(theme.clone());
                Some(Event::SetTheme(theme))
            }
            Message::SystemTheme(mode) => {
                let before = self.theme.variant();
                let generation = self.theme.set_system_mode(mode);
                (self.theme.variant() != before).then_some(Event::ThemeChanged(generation))
            }
        };
        (Task::none(), event)
    }

    /// The frame around `center`. `panel_body` draws the active tab's body while the panel is
    /// open; `map` lifts frame messages into the app's.
    pub fn view<'a, M: Clone + 'a>(&'a self, center: impl Into<Element<'a, M>>, panel_body: impl FnOnce(&'a str) -> Element<'a, M>, project_files: Option<Element<'a, M>>, map: fn(Message) -> M) -> Element<'a, M> {
        let full = self.full;
        let show = |pane| full.is_none() || full == Some(pane);
        let edge = |m| full.is_none().then(|| Element::from(splitter_edge(m)).map(map));
        let mut frame = row![];
        if show(Pane::Sidebar) {
            let width = if full == Some(Pane::Sidebar) { Length::Fill } else { Length::Fixed(self.sidebar_width) };
            frame = frame.push(container(self.sidebar.view_with(move |m| map(Message::Sidebar(m)), project_files)).width(width).height(Fill));
        }
        frame = frame.push(edge(Message::SidebarDragStart));
        if show(Pane::Center) {
            let strip = self.project_tabs.then(|| column![self.project_tabs_view().map(map), rule::horizontal(1).style(style::rule)]);
            let center = container(center).style(style::container(Surface::Ground)).width(Fill).height(Fill);
            frame = frame.push(column![strip, self.topbar.view().map(map), center].width(Fill));
        }
        if self.panel_open && show(Pane::Panel) {
            frame = frame.push(edge(Message::DragStart)).push(self.panel_view(panel_body, map));
        }
        let mut tracked = mouse_area(frame);
        if self.dragging.is_some() {
            tracked = tracked.on_move(move |p| map(Message::DragMove(p.x))).on_release(map(Message::DragEnd)).on_exit(map(Message::DragEnd)).interaction(mouse::Interaction::ResizingHorizontally);
        }
        sensor(tracked).on_show(move |s| map(Message::FrameSized(s))).on_resize(move |s| map(Message::FrameSized(s))).into()
    }

    /// The strip of open projects above the top bar: a pill per project with a close button
    /// that only drops the tab.
    fn project_tabs_view(&self) -> Element<'_, Message> {
        let active = self.active_project.as_deref();
        let tabs = tab_strip(self.open_projects.iter().map(|p| {
            let close = button(tinted(Icon::X, T::Caption, Tone::Muted)).on_press(Message::CloseProjectTab(p.clone())).style(style::button(ButtonKind::Ghost)).padding(sp(Sp::Xs) / 2.0);
            Tab { label: p.as_str(), icon: None, active: active == Some(p.as_str()), on: Message::OpenProjectTab(p.clone()), id: None, trailing: Some(close.into()) }
        }));
        let tabs = mouse_area(scrollable(tabs).id(PROJECT_TABS_ID).direction(tab_scroll()).style(style::scrollbar(true)).width(Fill)).on_scroll(Message::ProjectTabsWheel);
        container(tabs).style(style::container(Surface::Side)).padding(pad_x(Sp::S)).center_y(PROJECT_TABS_HEIGHT).width(Fill).into()
    }

    fn panel_view<'a, M: 'a>(&'a self, panel_body: impl FnOnce(&'a str) -> Element<'a, M>, map: fn(Message) -> M) -> Element<'a, M> {
        let active = self.active_tab();
        let tabs = tab_strip(self.tabs.iter().map(|t| Tab {
            label: t.title.as_str(),
            icon: t.icon,
            active: active == Some(t.id.as_str()),
            on: Message::SelectTab(t.id.clone()),
            id: None,
            trailing: self.is_detached(&t.id).then(|| tinted(Icon::ExternalLink, T::Caption, Tone::Muted).into()),
        }));
        // The scrollbar sits below the tabs, so it never covers one.
        let tabs = mouse_area(scrollable(tabs).id(PANEL_TABS_ID).direction(tab_scroll()).style(style::scrollbar(true)).width(Fill)).on_scroll(Message::TabsWheel);
        let is_full = self.full == Some(Pane::Panel);
        let full = icon_button(if is_full { Icon::Minimize } else { Icon::Maximize }, if is_full { "Restore panel" } else { "Fill window" }, Some(Message::ToggleFull(Pane::Panel)));
        let detach = active.filter(|id| !self.is_detached(id)).map(|id| container(icon_button(Icon::ExternalLink, "Open in a window", Some(Message::Detach(id.to_owned())))).id(PANEL_DETACH_ID));
        let controls = row![tabs, detach, full].spacing(sp(Sp::Xs)).align_y(Alignment::Center);
        let tab_bar = Element::from(container(controls).style(style::container(Surface::Side)).padding(pad_x(Sp::S)).center_y(topbar::HEIGHT).width(Fill)).map(map);
        let body = match active {
            Some(id) if self.is_detached(id) => {
                let title = self.tabs.iter().find(|t| t.id == id).map_or(id, |t| t.title.as_str());
                let action = |s, kind, on| button(label(s, T::Label, Tone::Normal)).on_press(on).style(style::button(kind)).padding([sp(Sp::Xs) * 1.5, sp(Sp::M)]);
                let actions = row![action("Show window", ButtonKind::Secondary, Message::ShowDetached(id.to_owned())), action("Reattach here", ButtonKind::Primary, Message::Reattach(id.to_owned()))];
                let note = column![tinted(Icon::ExternalLink, T::Title, Tone::Muted), label(format!("{title} is in its own window"), T::Body, Tone::Normal), actions.spacing(sp(Sp::S))];
                Element::from(container(note.spacing(sp(Sp::M)).align_x(Alignment::Center)).center(Fill)).map(map)
            }
            Some(id) => panel_body(id),
            None => Element::from(container(label("Nothing to show", T::Body, Tone::Muted)).center_x(Fill).padding(sp(Sp::Xl))).map(map),
        };
        let width = if is_full { Length::Fill } else { Length::Fixed(self.panel_width.min(self.max_panel_width())) };
        container(column![tab_bar, rule::horizontal(1).style(style::rule), container(body).width(Fill).height(Fill)]).style(style::container(Surface::Side)).width(width).height(Fill).into()
    }
}

/// A thin sideways scrollbar under a tab strip.
fn tab_scroll() -> scrollable::Direction {
    let w = sp(Sp::Xs) / 2.0;
    scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(w).scroller_width(w).spacing(iced::Pixels::ZERO))
}

/// Scrolls tab strip `id` sideways for a vertical wheel turn.
fn wheel_tabs(id: &'static str, delta: mouse::ScrollDelta) -> Task<Message> {
    let x = match delta {
        mouse::ScrollDelta::Lines { y, .. } => -y * WHEEL_LINE,
        mouse::ScrollDelta::Pixels { y, .. } => -y,
    };
    if x == 0.0 { Task::none() } else { operation::scroll_by(id, AbsoluteOffset { x, y: 0.0 }) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> State {
        let (mut state, _) = State::new(Appearance::default());
        state.set_tabs(["diff", "files", "terminal"].map(|t| PanelTab::new(t, t, None)).to_vec());
        let _ = state.update(Message::FrameSized(Size::new(1600.0, 900.0)));
        state
    }

    fn send(state: &mut State, messages: impl IntoIterator<Item = Message>) -> Vec<Event> { messages.into_iter().filter_map(|m| state.update(m).1).collect() }

    #[test]
    fn panel_and_sidebar_resize_within_bounds_and_keep_width_and_tab_while_closed() {
        let mut state = frame();
        send(&mut state, [Message::SelectTab("files".into()), Message::SelectTab("gone".into()), Message::DragStart, Message::DragMove(1100.0), Message::DragEnd, Message::TogglePanel]);
        assert!(!state.panel_open());
        send(&mut state, [Message::TogglePanel]);
        assert_eq!((state.panel_width, state.active_tab()), (497.0, Some("files")));
        send(&mut state, [Message::DragStart, Message::DragMove(10.0)]);
        assert_eq!(state.panel_width, 1600.0 - SIDEBAR_WIDTH - MIN_CENTER_WIDTH - 2.0 * splitter_width());
        send(&mut state, [Message::DragMove(1590.0), Message::DragEnd, Message::DragMove(600.0)]);
        assert_eq!(state.panel_width, MIN_PANEL_WIDTH, "moves after release do not resize");
        for (x, width) in [(343.0, 340.0), (5000.0, MAX_SIDEBAR_WIDTH), (10.0, MIN_SIDEBAR_WIDTH)] {
            send(&mut state, [Message::SidebarDragStart, Message::DragMove(x), Message::DragEnd]);
            assert_eq!(state.sidebar_width, width);
        }
        state.show_tab("terminal");
        assert_eq!(state.active_tab(), Some("terminal"));
    }

    #[test]
    fn panes_go_full_one_at_a_time_and_events_pass_through() {
        let mut state = frame();
        send(&mut state, [Message::ToggleFull(Pane::Panel), Message::ToggleFull(Pane::Center)]);
        assert_eq!(state.full(), Some(Pane::Center), "the chat takes over");
        send(&mut state, [Message::ToggleFull(Pane::Center), Message::ToggleFull(Pane::Panel), Message::TogglePanel]);
        assert_eq!(state.full(), None, "closing the panel leaves its full screen");
        let events = send(&mut state, [Message::OpenProjectTab("loa".into()), Message::CloseProjectTab("docs".into()), Message::Detach("terminal".into()), Message::Reattach("terminal".into())]);
        assert_eq!(events, [Event::Sidebar(sidebar::Event::OpenProject("loa".into())), Event::CloseProjectTab("docs".into()), Event::DetachTab("terminal".into()), Event::ReattachTab("terminal".into())]);
        let events = send(&mut state, [Message::SystemTheme(Mode::Light), Message::SetTheme(ThemeRef("dark".into()))]);
        assert!(matches!(events[..], [Event::ThemeChanged(_), Event::SetTheme(_)]));
        assert_eq!(state.theme_state().variant(), theme::Variant::Dark);
    }

    #[test]
    fn breadcrumb_follows_the_active_session_and_the_frame_renders_narrow() {
        let mut state = frame();
        state.set_nodes(vec![NodeStatus { key: "k-box".into(), name: "buildbox".into(), online: true, local: false }]);
        let mut session = SessionSummary { id: "s1".into(), title: "Ship it".into(), project: Some("loa".into()), node: "k-box".into(), pinned: false, waiting: false, updated: 1 };
        state.set_sessions([session.clone()]);
        assert_eq!(send(&mut state, [Message::Sidebar(sidebar::Message::OpenSession("s1".into()))]), [Event::Sidebar(sidebar::Event::OpenSession("s1".into()))]);
        session.waiting = true;
        state.upsert_session(session);
        let crumb = state.topbar().breadcrumb().unwrap();
        assert_eq!((crumb.title.as_str(), crumb.node.name.as_str(), crumb.waiting), ("Ship it", "buildbox", true));
        let view = state.view(iced::widget::space(), |_| iced::widget::space().into(), None, |m| m);
        assert!(iced_test::Simulator::with_size(iced::Settings::default(), Size::new(700.0, 500.0), view).find("Ship it").is_ok());
        state.remove_session("s1");
        assert!(state.topbar().breadcrumb().is_none());
    }
}
