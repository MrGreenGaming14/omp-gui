//! The app frame: sidebar, top bar over a center slot, and a resizable right panel with tabs.

use crate::data::NodeStatus;
use loa_contracts::session::SessionSummary;
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

const SIDEBAR_WIDTH: f32 = 272.0;
const SIDEBAR_WIDTHS: (f32, f32) = (200.0, 480.0);
const MIN_PANEL_WIDTH: f32 = 240.0;
const DEFAULT_PANEL_WIDTH: f32 = 420.0;
const MIN_CENTER_WIDTH: f32 = 320.0;
const PANEL_TABS_ID: &str = "loa-panel-tabs";
const PROJECT_TABS_ID: &str = "loa-project-tabs";
pub const PANEL_DETACH_ID: &str = "loa-panel-detach";
const PROJECT_TABS_HEIGHT: f32 = 34.0;
/// Pixels a mouse wheel line scrolls a tab strip.
const WHEEL_LINE: f32 = 40.0;

/// A pane that can fill the window, or whose edge is dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane { Sidebar, Center, Panel }

/// One tab of the right panel; the app draws the body of the active one by `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelTab { pub id: String, pub title: String, pub icon: Option<Icon> }

impl PanelTab {
    pub fn new(id: impl Into<String>, title: impl Into<String>, icon: Option<Icon>) -> Self { Self { id: id.into(), title: title.into(), icon } }
}

#[derive(Debug, Clone)]
pub struct State {
    sidebar: sidebar::State,
    theme: theme::State,
    pub(crate) status: Option<String>,
    panel_open: bool,
    panel_width: f32,
    tabs: Vec<PanelTab>,
    /// The tab the user picked; may name a tab not added yet, then the first tab shows.
    active: Option<String>,
    frame: Option<Size>,
    project_tabs: bool,
    open_projects: Vec<String>,
    sidebar_width: f32,
    dragging: Option<Pane>,
    full: Option<Pane>,
    /// Tabs shown in their own window instead of the panel.
    detached: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Pass this event on as is.
    Emit(Event),
    Sidebar(sidebar::Message),
    SelectTab(String),
    TabsWheel(&'static str, mouse::ScrollDelta),
    DragStart(Pane),
    /// The cursor moved to this x while an edge is held.
    DragMove(f32),
    DragEnd,
    TogglePanel,
    ToggleFull(Pane),
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
        let state = Self {
            sidebar: sidebar::State::default(), theme: theme::State::new(appearance), status: None, panel_open: true, panel_width: DEFAULT_PANEL_WIDTH, tabs: Vec::new(), active: None,
            frame: None, project_tabs: false, open_projects: Vec::new(), sidebar_width: SIDEBAR_WIDTH, dragging: None, full: None, detached: Vec::new(),
        };
        (state, iced::system::theme().map(Message::SystemTheme))
    }
    pub fn subscription(&self) -> Subscription<Message> { iced::system::theme_changes().map(Message::SystemTheme) }
    pub fn theme(&self) -> Theme { self.theme.theme() }
    pub fn theme_state(&self) -> &theme::State { &self.theme }
    /// Replaces the appearance (theme and overrides), e.g. after the config changed.
    pub fn set_appearance(&mut self, appearance: Appearance) -> Option<Event> {
        (self.theme.appearance() != &appearance).then(|| Event::ThemeChanged(self.theme.set_appearance(appearance)))
    }
    pub fn sidebar(&self) -> &sidebar::State { &self.sidebar }
    pub fn sidebar_mut(&mut self) -> &mut sidebar::State { &mut self.sidebar }
    /// Highlights the selected project in the sidebar and the project tab strip.
    pub fn set_active_project(&mut self, project: Option<String>) { self.sidebar.active_project = project }
    pub fn set_shown_project(&mut self, project: Option<String>) { self.sidebar.shown_project = project }
    pub fn set_project_tabs(&mut self, enabled: bool) { self.project_tabs = enabled }
    pub fn project_tabs(&self) -> bool { self.project_tabs }
    pub fn set_open_projects(&mut self, projects: Vec<String>) { self.open_projects = projects }
    pub fn open_projects(&self) -> &[String] { &self.open_projects }
    pub fn full(&self) -> Option<Pane> { self.full }
    pub fn set_detached(&mut self, tabs: Vec<String>) { self.detached = tabs }
    pub fn is_detached(&self, tab: &str) -> bool { self.detached.iter().any(|d| d == tab) }
    /// The short status of the shown session in the top bar, e.g. "Running".
    pub fn set_session_status(&mut self, status: Option<String>) { self.status = status }
    /// Replaces the panel tabs; the active tab stays when it is still there.
    pub fn set_tabs(&mut self, tabs: Vec<PanelTab>) { self.tabs = tabs }
    pub fn tabs(&self) -> &[PanelTab] { &self.tabs }
    pub fn panel_open(&self) -> bool { self.panel_open }
    /// The active session and its node, as the top bar shows them.
    pub fn breadcrumb(&self) -> Option<(&SessionSummary, NodeStatus)> {
        let s = self.sidebar.active().and_then(|id| self.sidebar.session(id))?;
        let nodes = self.sidebar.nodes();
        Some((s, nodes.get(&s.node).cloned().unwrap_or_else(|| NodeStatus { key: s.node.clone(), name: nodes.name_of(&s.node).to_owned(), online: false, local: false })))
    }
    /// Adds a tab, or replaces the tab with the same id.
    pub fn add_tab(&mut self, tab: PanelTab) {
        match self.tabs.iter_mut().find(|t| t.id == tab.id) {
            Some(t) => *t = tab,
            None => self.tabs.push(tab),
        }
    }
    /// Opens the panel on this tab; pair it with [`State::reveal_active_tab`].
    pub fn show_tab(&mut self, id: &str) {
        if self.tabs.iter().any(|t| t.id == id) {
            self.active = Some(id.to_owned());
            self.set_panel_open(true);
        }
    }
    /// Snaps an overflowing tab bar to the active tab's share of it, so the first and last tabs land at its ends.
    pub fn reveal_active_tab<M: Send + 'static>(&self) -> Task<M> {
        let index = self.active_tab().and_then(|a| self.tabs.iter().position(|t| t.id == a)).unwrap_or(0);
        let x = if self.tabs.len() < 2 { 0.0 } else { index as f32 / (self.tabs.len() - 1) as f32 };
        operation::snap_to(PANEL_TABS_ID, RelativeOffset { x: Some(x), y: None })
    }
    pub fn active_tab(&self) -> Option<&str> {
        self.active.as_deref().filter(|a| self.tabs.iter().any(|t| t.id == *a)).or_else(|| self.tabs.first().map(|t| t.id.as_str()))
    }
    /// Opens or closes the panel, keeping width and tab; closing leaves its full screen.
    pub fn set_panel_open(&mut self, open: bool) {
        self.panel_open = open;
        if self.dragging == Some(Pane::Panel) { self.dragging = None }
        if !open && self.full == Some(Pane::Panel) { self.full = None }
    }
    fn max_panel_width(&self) -> f32 {
        self.frame.map_or(f32::INFINITY, |f| (f.width - self.sidebar_width - MIN_CENTER_WIDTH - 2.0 * splitter_width()).max(MIN_PANEL_WIDTH))
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match message {
            Message::Sidebar(m) => {
                let (task, event) = self.sidebar.update(m);
                return (task.map(Message::Sidebar), event.map(Event::Sidebar));
            }
            Message::TabsWheel(id, delta) => return (wheel_tabs(id, delta), None),
            Message::Emit(event) => return (Task::none(), Some(event)),
            Message::SetTheme(theme) => {
                self.theme.set_preference(theme.clone());
                return (Task::none(), Some(Event::SetTheme(theme)));
            }
            Message::SystemTheme(mode) => {
                let before = self.theme.variant();
                let generation = self.theme.set_system_mode(mode);
                return (Task::none(), (self.theme.variant() != before).then_some(Event::ThemeChanged(generation)));
            }
            Message::SelectTab(id) => if self.tabs.iter().any(|t| t.id == id) { self.active = Some(id) },
            Message::DragStart(pane) => self.dragging = (pane != Pane::Panel || self.panel_open).then_some(pane),
            Message::DragMove(x) => match (self.dragging, self.frame) {
                (Some(Pane::Panel), Some(f)) => self.panel_width = (f.width - x - splitter_width() / 2.0).min(self.max_panel_width()).max(MIN_PANEL_WIDTH),
                (Some(Pane::Sidebar), _) => self.sidebar_width = (x - splitter_width() / 2.0).clamp(SIDEBAR_WIDTHS.0, SIDEBAR_WIDTHS.1),
                _ => {}
            },
            Message::DragEnd => self.dragging = None,
            Message::TogglePanel => self.set_panel_open(!self.panel_open),
            Message::ToggleFull(pane) => self.full = (self.full != Some(pane)).then_some(pane),
            Message::FrameSized(size) => self.frame = Some(size),
        }
        (Task::none(), None)
    }

    /// The frame around `center`; `panel_body` draws the active tab's body and `map` lifts frame messages into the app's.
    pub fn view<'a, M: Clone + 'a>(&'a self, center: impl Into<Element<'a, M>>, panel_body: impl FnOnce(&'a str) -> Element<'a, M>, project_files: Option<Element<'a, M>>, map: fn(Message) -> M) -> Element<'a, M> {
        let full = self.full;
        let show = |pane| full.is_none() || full == Some(pane);
        let edge = |pane| full.is_none().then(|| Element::from(splitter_edge(Message::DragStart(pane))).map(map));
        let mut frame = row![];
        if show(Pane::Sidebar) {
            let width = if full == Some(Pane::Sidebar) { Length::Fill } else { Length::Fixed(self.sidebar_width) };
            frame = frame.push(container(self.sidebar.view_with(move |m| map(Message::Sidebar(m)), project_files)).width(width).height(Fill));
        }
        frame = frame.push(edge(Pane::Sidebar));
        if show(Pane::Center) {
            let strip = self.project_tabs.then(|| column![self.project_tabs_view().map(map), rule::horizontal(1).style(style::rule)]);
            let center = container(center).style(style::container(Surface::Ground)).width(Fill).height(Fill);
            frame = frame.push(column![strip, topbar::view(self).map(map), center].width(Fill));
        }
        if self.panel_open && show(Pane::Panel) {
            frame = frame.push(edge(Pane::Panel)).push(self.panel_view(panel_body, map));
        }
        let mut tracked = mouse_area(frame);
        if self.dragging.is_some() {
            tracked = tracked.on_move(move |p| map(Message::DragMove(p.x))).on_release(map(Message::DragEnd)).on_exit(map(Message::DragEnd)).interaction(mouse::Interaction::ResizingHorizontally);
        }
        sensor(tracked).on_show(move |s| map(Message::FrameSized(s))).on_resize(move |s| map(Message::FrameSized(s))).into()
    }

    /// The strip of open projects above the top bar; closing a tab only drops the tab.
    fn project_tabs_view(&self) -> Element<'_, Message> {
        let active = self.sidebar.active_project.as_deref();
        let tabs = tab_strip(self.open_projects.iter().map(|p| {
            let close = button(tinted(Icon::X, T::Caption, Tone::Muted)).on_press(Message::Emit(Event::CloseProjectTab(p.clone()))).style(style::button(ButtonKind::Ghost)).padding(sp(Sp::Xs) / 2.0);
            Tab { label: p.as_str(), icon: None, active: active == Some(p.as_str()), on: open_project(p), id: None, trailing: Some(close.into()) }
        }));
        container(scrolling_strip(PROJECT_TABS_ID, tabs)).style(style::container(Surface::Side)).padding(pad_x(Sp::S)).center_y(PROJECT_TABS_HEIGHT).width(Fill).into()
    }

    fn panel_view<'a, M: 'a>(&'a self, panel_body: impl FnOnce(&'a str) -> Element<'a, M>, map: fn(Message) -> M) -> Element<'a, M> {
        let active = self.active_tab();
        let tabs = tab_strip(self.tabs.iter().map(|t| {
            let trailing = self.is_detached(&t.id).then(|| tinted(Icon::ExternalLink, T::Caption, Tone::Muted).into());
            Tab { label: t.title.as_str(), icon: t.icon, active: active == Some(t.id.as_str()), on: Message::SelectTab(t.id.clone()), id: None, trailing }
        }));
        let is_full = self.full == Some(Pane::Panel);
        let full = icon_button(if is_full { Icon::Minimize } else { Icon::Maximize }, if is_full { "Restore panel" } else { "Fill window" }, Some(Message::ToggleFull(Pane::Panel)));
        let detach = active.filter(|id| !self.is_detached(id)).map(|id| container(icon_button(Icon::ExternalLink, "Open in a window", Some(Message::Emit(Event::DetachTab(id.to_owned()))))).id(PANEL_DETACH_ID));
        let controls = row![scrolling_strip(PANEL_TABS_ID, tabs), detach, full].spacing(sp(Sp::Xs)).align_y(Alignment::Center);
        let tab_bar = Element::from(container(controls).style(style::container(Surface::Side)).padding(pad_x(Sp::S)).center_y(topbar::HEIGHT).width(Fill)).map(map);
        let body = match active {
            Some(id) if self.is_detached(id) => {
                let title = self.tabs.iter().find(|t| t.id == id).map_or(id, |t| t.title.as_str());
                let action = |s, kind, on| button(label(s, T::Label, Tone::Normal)).on_press(Message::Emit(on)).style(style::button(kind)).padding([sp(Sp::Xs) * 1.5, sp(Sp::M)]);
                let actions = row![action("Show window", ButtonKind::Secondary, Event::ShowDetached(id.to_owned())), action("Reattach here", ButtonKind::Primary, Event::ReattachTab(id.to_owned()))];
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

pub(crate) fn open_project(project: &str) -> Message { Message::Emit(Event::Sidebar(sidebar::Event::OpenProject(project.to_owned()))) }

/// A tab strip that scrolls sideways under the wheel, with a thin scrollbar below the tabs so it never covers one.
fn scrolling_strip<'a>(id: &'static str, tabs: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    let w = sp(Sp::Xs) / 2.0;
    let direction = scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(w).scroller_width(w).spacing(iced::Pixels::ZERO));
    mouse_area(scrollable(tabs).id(id).direction(direction).style(style::scrollbar(true)).width(Fill)).on_scroll(move |d| Message::TabsWheel(id, d)).into()
}

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

    fn send(state: &mut State, messages: impl IntoIterator<Item = Message>) -> Vec<Event> { messages.into_iter().filter_map(|m| state.update(m).1).collect() }

    #[test]
    fn panes_resize_within_bounds_go_full_and_follow_the_theme() {
        let (mut state, _) = State::new(Appearance::default());
        state.set_tabs(["diff", "files"].map(|t| PanelTab::new(t, t, None)).to_vec());
        send(&mut state, [Message::FrameSized(Size::new(1600.0, 900.0)), Message::SelectTab("files".into()), Message::SelectTab("gone".into())]);
        let max_panel = 1600.0 - SIDEBAR_WIDTH - MIN_CENTER_WIDTH - 2.0 * splitter_width();
        let drags = [(Pane::Panel, 1100.0, 497.0), (Pane::Panel, 10.0, max_panel), (Pane::Panel, 1590.0, MIN_PANEL_WIDTH), (Pane::Sidebar, 343.0, 340.0), (Pane::Sidebar, 5000.0, 480.0), (Pane::Sidebar, 10.0, 200.0)];
        for (pane, x, width) in drags {
            send(&mut state, [Message::DragStart(pane), Message::DragMove(x), Message::DragEnd, Message::DragMove(600.0)]);
            assert_eq!(if pane == Pane::Panel { state.panel_width } else { state.sidebar_width }, width, "{pane:?} to {x}");
        }
        send(&mut state, [Message::TogglePanel, Message::TogglePanel, Message::ToggleFull(Pane::Panel), Message::ToggleFull(Pane::Center)]);
        assert_eq!((state.active_tab(), state.full()), (Some("files"), Some(Pane::Center)), "the panel keeps its tab and the chat takes over");
        send(&mut state, [Message::ToggleFull(Pane::Panel), Message::TogglePanel]);
        assert_eq!(state.full(), None, "closing the panel leaves its full screen");
        let events = send(&mut state, [Message::SystemTheme(Mode::Light), Message::SetTheme(ThemeRef("dark".into())), Message::SetTheme(ThemeRef("nope".into()))]);
        assert!(matches!(events[..], [Event::ThemeChanged(_), Event::SetTheme(_), Event::SetTheme(_)]));
        assert_eq!((state.theme_state().variant(), state.theme_state().errors().len()), (theme::Variant::Light, 1), "unknown themes fall back to the system");
        let dark = Appearance { theme: ThemeRef("dark".into()), ..Appearance::default() };
        assert!(matches!(state.set_appearance(dark.clone()), Some(Event::ThemeChanged(_))) && state.set_appearance(dark).is_none(), "a config change re-applies the appearance once");
    }

    #[test]
    fn breadcrumb_follows_the_active_session_and_the_frame_renders_narrow() {
        let (mut state, _) = State::new(Appearance::default());
        state.sidebar.set_nodes(vec![NodeStatus { key: "k-box".into(), name: "buildbox".into(), online: true, local: false }]);
        state.sidebar.set_sessions([SessionSummary { id: "s1".into(), title: "Ship it".into(), project: Some("loa".into()), node: "k-box".into(), pinned: false, waiting: true, updated: 1 }]);
        assert_eq!(send(&mut state, [Message::Sidebar(sidebar::Message::OpenSession("s1".into()))]), [Event::Sidebar(sidebar::Event::OpenSession("s1".into()))]);
        let (crumb, node) = state.breadcrumb().unwrap();
        assert_eq!((crumb.title.as_str(), node.name.as_str(), crumb.waiting), ("Ship it", "buildbox", true));
        let view = state.view(iced::widget::space(), |_| iced::widget::space().into(), None, |m| m);
        assert!(iced_test::Simulator::with_size(iced::Settings::default(), Size::new(700.0, 500.0), view).find("Ship it").is_ok());
        state.sidebar.remove_session("s1");
        assert!(state.breadcrumb().is_none());
    }
}
