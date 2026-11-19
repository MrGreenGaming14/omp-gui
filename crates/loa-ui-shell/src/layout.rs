//! The app frame: sidebar on the left, top bar above a center slot, and a
//! toggleable right panel with a tab bar.
//!
//! The panel remembers its width and active tab while closed. Tabs are data:
//! the app adds diff, files, terminal, agents, session tree and surface tabs
//! and draws the body of the active one.

use crate::data::{NodeStatus, Nodes, SessionSummary, short_key};
use crate::theme::{self, Preference, style};
use crate::{sidebar, topbar};
use iced::theme::Mode;
use iced::widget::operation::{self, AbsoluteOffset, RelativeOffset};
use iced::widget::{button, column, container, mouse_area, row, rule, scrollable, sensor, space, text};
use iced::{Element, Fill, Shrink, Size, Subscription, Task, Theme, mouse};

/// Width of the sidebar.
pub const SIDEBAR_WIDTH: f32 = 264.0;

/// Narrowest the right panel gets.
pub const MIN_PANEL_WIDTH: f32 = 240.0;

/// Width of the right panel before the user resizes it.
pub const DEFAULT_PANEL_WIDTH: f32 = 420.0;

/// Narrowest the center slot gets while the panel is resized.
pub const MIN_CENTER_WIDTH: f32 = 320.0;

/// Container id of the right panel.
pub const PANEL_ID: &str = "loa-right-panel";

/// Id of the panel's tab bar, which scrolls sideways when the tabs overflow.
pub const PANEL_TABS_ID: &str = "loa-panel-tabs";

/// Pixels a mouse wheel line scrolls the tab bar.
const WHEEL_LINE: f32 = 40.0;

/// Width of the grab area between the center and the panel.
const HANDLE_WIDTH: f32 = 6.0;

/// One tab of the right panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelTab {
    /// Stable id the app uses to draw the body, e.g. `"diff"`.
    pub id: String,
    /// Label on the tab.
    pub title: String,
}

impl PanelTab {
    /// A tab with this id and label.
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self { id: id.into(), title: title.into() }
    }
}

/// Panel settings worth keeping across restarts.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelPrefs {
    /// Whether the panel is open.
    pub open: bool,
    /// Width in logical pixels.
    pub width: f32,
    /// Id of the active tab.
    pub active: Option<String>,
}

impl Default for PanelPrefs {
    fn default() -> Self {
        Self { open: true, width: DEFAULT_PANEL_WIDTH, active: None }
    }
}

#[derive(Debug, Clone)]
struct Panel {
    open: bool,
    width: f32,
    tabs: Vec<PanelTab>,
    /// The tab the user picked. May name a tab not added yet (restored
    /// prefs); [`State::active_tab`] falls back to the first tab then.
    active: Option<String>,
    dragging: bool,
}

/// Frame state: the sidebar, top bar, panel and theme.
#[derive(Debug, Clone)]
pub struct State {
    sidebar: sidebar::State,
    topbar: topbar::State,
    panel: Panel,
    theme: theme::State,
    frame: Option<Size>,
}

/// Internal messages.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// From the sidebar.
    Sidebar(sidebar::Message),
    /// From the top bar.
    Topbar(topbar::Message),
    /// A panel tab was clicked.
    SelectTab(String),
    /// The mouse wheel turned over the tab bar.
    TabsWheel(mouse::ScrollDelta),
    /// The panel edge was grabbed.
    DragStart,
    /// The cursor moved to this x while the panel edge is held.
    DragMove(f32),
    /// The panel edge was released.
    DragEnd,
    /// The frame was laid out at this size.
    FrameSized(Size),
    /// The user picked a theme.
    SetTheme(Preference),
    /// The system theme changed.
    SystemTheme(Mode),
}

/// What the app must act on.
#[derive(Debug, Clone, PartialEq)]
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
    /// Delete this session and its journal.
    DeleteSession(String),
    /// The right panel opened or closed.
    PanelToggled {
        /// Whether it is open now.
        open: bool,
    },
    /// A panel tab became active.
    SelectTab(String),
    /// The user finished resizing the panel to this width.
    PanelResized(f32),
    /// The user picked a theme. Persist it.
    SetTheme(Preference),
}

impl From<sidebar::Event> for Event {
    fn from(event: sidebar::Event) -> Self {
        match event {
            sidebar::Event::NewSession => Event::NewSession,
            sidebar::Event::OpenSession(id) => Event::OpenSession(id),
            sidebar::Event::OpenProject(project) => Event::OpenProject(project),
            sidebar::Event::OpenPeers => Event::OpenPeers,
            sidebar::Event::OpenTools => Event::OpenTools,
            sidebar::Event::OpenUpdates => Event::OpenUpdates,
            sidebar::Event::SetPinned { id, pinned } => Event::SetPinned { id, pinned },
            sidebar::Event::DeleteSession(id) => Event::DeleteSession(id),
        }
    }
}

impl State {
    /// Builds the frame with the saved theme preference and panel settings.
    /// The task asks the system for its theme.
    pub fn new(preference: Preference, panel: PanelPrefs) -> (Self, Task<Message>) {
        let mut topbar = topbar::State::default();
        topbar.set_panel_open(panel.open);
        let state = Self {
            sidebar: sidebar::State::default(),
            topbar,
            panel: Panel {
                open: panel.open,
                width: panel.width.max(MIN_PANEL_WIDTH),
                tabs: Vec::new(),
                active: panel.active,
                dragging: false,
            },
            theme: theme::State::new(preference),
            frame: None,
        };
        (state, theme::State::detect().map(Message::SystemTheme))
    }

    /// System theme changes.
    pub fn subscription(&self) -> Subscription<Message> {
        theme::State::subscription().map(Message::SystemTheme)
    }

    /// The theme to return from the application's `theme` function.
    pub fn theme(&self) -> Theme {
        self.theme.theme()
    }

    /// The user's theme preference.
    pub fn theme_preference(&self) -> Preference {
        self.theme.preference()
    }

    /// The sidebar, for reading its lists.
    pub fn sidebar(&self) -> &sidebar::State {
        &self.sidebar
    }

    /// The top bar, for reading its breadcrumb.
    pub fn topbar(&self) -> &topbar::State {
        &self.topbar
    }

    /// Replaces every session.
    pub fn set_sessions(&mut self, sessions: impl IntoIterator<Item = SessionSummary>) {
        self.sidebar.set_sessions(sessions);
        self.refresh_breadcrumb();
    }

    /// Adds a session or replaces the one with the same id.
    pub fn upsert_session(&mut self, summary: SessionSummary) {
        self.sidebar.upsert_session(summary);
        self.refresh_breadcrumb();
    }

    /// Drops a session.
    pub fn remove_session(&mut self, id: &str) {
        self.sidebar.remove_session(id);
        self.refresh_breadcrumb();
    }

    /// Replaces the pinned session ids, as saved by the app.
    pub fn set_pins(&mut self, pins: impl IntoIterator<Item = String>) {
        self.sidebar.set_pins(pins);
    }

    /// Replaces the project list.
    pub fn set_projects(&mut self, projects: impl IntoIterator<Item = String>) {
        self.sidebar.set_projects(projects);
    }

    /// Replaces the node list.
    pub fn set_nodes(&mut self, nodes: Vec<NodeStatus>) {
        self.sidebar.set_nodes(Nodes::new(nodes));
        self.refresh_breadcrumb();
    }

    /// Sets how many updates wait for the user; the sidebar lists an
    /// Updates row while it is non-zero.
    pub fn set_updates(&mut self, count: usize) {
        self.sidebar.set_updates(count);
    }

    /// How many updates wait for the user.
    pub fn updates(&self) -> usize {
        self.sidebar.updates()
    }

    /// Sets the session shown in the center.
    pub fn set_active_session(&mut self, id: Option<String>) {
        self.sidebar.set_active(id);
        self.refresh_breadcrumb();
    }

    /// Sets the short status of the session shown in the top bar, e.g.
    /// "Running", "Waiting" or "Interrupted", or `None` to hide it.
    pub fn set_session_status(&mut self, status: Option<String>) {
        self.topbar.set_status(status);
    }

    fn refresh_breadcrumb(&mut self) {
        let crumb = self.sidebar.active().and_then(|id| self.sidebar.session(id)).map(|session| {
            let node = self.sidebar.nodes().get(&session.node).cloned().unwrap_or_else(|| NodeStatus {
                key: session.node.clone(),
                name: short_key(&session.node).to_owned(),
                online: false,
                local: false,
            });
            topbar::Breadcrumb {
                project: session.project.clone(),
                title: session.title.clone(),
                waiting: session.waiting,
                node,
            }
        });
        self.topbar.set_breadcrumb(crumb);
    }

    /// Replaces the panel tabs. The active tab stays when it is still there.
    pub fn set_tabs(&mut self, tabs: Vec<PanelTab>) {
        self.panel.tabs = tabs;
    }

    /// Adds a tab, or renames the tab with the same id.
    pub fn add_tab(&mut self, tab: PanelTab) {
        match self.panel.tabs.iter_mut().find(|existing| existing.id == tab.id) {
            Some(existing) => *existing = tab,
            None => self.panel.tabs.push(tab),
        }
    }

    /// Removes a tab. When it was active, the first tab takes over.
    pub fn remove_tab(&mut self, id: &str) {
        self.panel.tabs.retain(|tab| tab.id != id);
        if self.panel.active.as_deref() == Some(id) {
            self.panel.active = None;
        }
    }

    /// The panel tabs in order.
    pub fn tabs(&self) -> &[PanelTab] {
        &self.panel.tabs
    }

    /// Opens the panel on this tab, e.g. when a file is clicked in the chat.
    /// Pair it with [`State::reveal_active_tab`] so the tab bar scrolls to
    /// it when the tabs overflow.
    pub fn show_tab(&mut self, id: &str) {
        if self.panel.tabs.iter().any(|tab| tab.id == id) {
            self.panel.active = Some(id.to_owned());
            self.set_panel_open(true);
        }
    }

    /// Scrolls an overflowing tab bar so the active tab shows: the bar snaps
    /// to the active tab's share of the bar, so the first and last tabs land
    /// exactly at the start and the end.
    pub fn reveal_active_tab<T: Send + 'static>(&self) -> Task<T> {
        let tabs = &self.panel.tabs;
        let index = self.active_tab().and_then(|active| tabs.iter().position(|tab| tab.id == active)).unwrap_or(0);
        let x = match tabs.len() {
            0 | 1 => 0.0,
            len => index as f32 / (len - 1) as f32,
        };
        operation::snap_to(PANEL_TABS_ID, RelativeOffset { x: Some(x), y: None })
    }

    /// The id of the tab whose body the panel shows.
    pub fn active_tab(&self) -> Option<&str> {
        let tabs = &self.panel.tabs;
        self.panel
            .active
            .as_deref()
            .filter(|active| tabs.iter().any(|tab| tab.id == *active))
            .or_else(|| tabs.first().map(|tab| tab.id.as_str()))
    }

    /// Whether the panel is open.
    pub fn panel_open(&self) -> bool {
        self.panel.open
    }

    /// Opens or closes the panel. Width and active tab are kept.
    pub fn set_panel_open(&mut self, open: bool) {
        self.panel.open = open;
        self.panel.dragging = false;
        self.topbar.set_panel_open(open);
    }

    /// The panel width the user chose.
    pub fn panel_width(&self) -> f32 {
        self.panel.width
    }

    /// The panel settings to persist.
    pub fn panel_prefs(&self) -> PanelPrefs {
        PanelPrefs { open: self.panel.open, width: self.panel.width, active: self.panel.active.clone() }
    }

    /// Widest the panel may be in the current frame.
    fn max_panel_width(&self) -> f32 {
        self.frame.map_or(f32::INFINITY, |frame| {
            (frame.width - SIDEBAR_WIDTH - MIN_CENTER_WIDTH - HANDLE_WIDTH).max(MIN_PANEL_WIDTH)
        })
    }

    /// Handles a message.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match message {
            Message::Sidebar(message) => {
                let (task, event) = self.sidebar.update(message);
                if matches!(event, Some(sidebar::Event::OpenSession(_))) {
                    self.refresh_breadcrumb();
                }
                (task.map(Message::Sidebar), event.map(Event::from))
            }
            Message::Topbar(message) => {
                let (task, event) = self.topbar.update(message);
                let event = event.map(|event| match event {
                    topbar::Event::OpenProject(project) => Event::OpenProject(project),
                    topbar::Event::TogglePanel => {
                        self.set_panel_open(!self.panel.open);
                        Event::PanelToggled { open: self.panel.open }
                    }
                });
                (task.map(Message::Topbar), event)
            }
            Message::SelectTab(id) => {
                let known = self.panel.tabs.iter().any(|tab| tab.id == id);
                let event = known.then(|| {
                    self.panel.active = Some(id.clone());
                    Event::SelectTab(id)
                });
                (Task::none(), event)
            }
            Message::TabsWheel(delta) => {
                // The tab bar scrolls only sideways: a vertical wheel turns
                // it, sideways deltas reach the scrollable itself.
                let x = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => -y * WHEEL_LINE,
                    mouse::ScrollDelta::Pixels { y, .. } => -y,
                };
                let task = if x == 0.0 { Task::none() } else { operation::scroll_by(PANEL_TABS_ID, AbsoluteOffset { x, y: 0.0 }) };
                (task, None)
            }
            Message::DragStart => {
                self.panel.dragging = self.panel.open;
                (Task::none(), None)
            }
            Message::DragMove(x) => {
                if self.panel.dragging
                    && let Some(frame) = self.frame
                {
                    let width = frame.width - x - HANDLE_WIDTH / 2.0;
                    self.panel.width = width.min(self.max_panel_width()).max(MIN_PANEL_WIDTH);
                }
                (Task::none(), None)
            }
            Message::DragEnd => {
                let event = std::mem::take(&mut self.panel.dragging).then_some(Event::PanelResized(self.panel.width));
                (Task::none(), event)
            }
            Message::FrameSized(size) => {
                self.frame = Some(size);
                (Task::none(), None)
            }
            Message::SetTheme(preference) => {
                self.theme.set_preference(preference);
                (Task::none(), Some(Event::SetTheme(preference)))
            }
            Message::SystemTheme(mode) => {
                self.theme.set_system_mode(mode);
                (Task::none(), None)
            }
        }
    }

    /// Draws the frame around the app's content.
    ///
    /// `center` fills the area under the top bar. `panel_body` draws the body
    /// of the active tab and is only called while the panel is open with a
    /// tab. `map` lifts this crate's messages into the app's.
    pub fn view<'a, M: Clone + 'a>(
        &'a self,
        center: impl Into<Element<'a, M>>,
        panel_body: impl FnOnce(&'a str) -> Element<'a, M>,
        map: fn(Message) -> M,
    ) -> Element<'a, M> {
        let sidebar = container(self.sidebar.view().map(Message::Sidebar)).width(SIDEBAR_WIDTH).height(Fill);
        let sidebar = Element::from(sidebar).map(map);

        let topbar = self.topbar.view().map(Message::Topbar).map(map);
        let center = container(center).style(style::surface).width(Fill).height(Fill);

        let mut frame = row![
            sidebar,
            rule::vertical(1).style(style::divider),
            column![topbar, rule::horizontal(1).style(style::divider), center],
        ];
        if self.panel.open {
            let handle =
                mouse_area(container(rule::vertical(1).style(style::divider)).center_x(HANDLE_WIDTH).height(Fill))
                    .on_press(Message::DragStart)
                    .interaction(mouse::Interaction::ResizingHorizontally);
            frame = frame.push(Element::from(handle).map(map));
            frame = frame.push(self.panel_view(panel_body, map));
        }

        let mut tracked = mouse_area(frame);
        if self.panel.dragging {
            tracked = tracked
                .on_move(move |point| map(Message::DragMove(point.x)))
                .on_release(map(Message::DragEnd))
                .on_exit(map(Message::DragEnd))
                .interaction(mouse::Interaction::ResizingHorizontally);
        }

        sensor(tracked)
            .on_show(move |size| map(Message::FrameSized(size)))
            .on_resize(move |size| map(Message::FrameSized(size)))
            .into()
    }

    fn panel_view<'a, M: 'a>(
        &'a self,
        panel_body: impl FnOnce(&'a str) -> Element<'a, M>,
        map: fn(Message) -> M,
    ) -> Element<'a, M> {
        let active = self.active_tab();
        let tabs = self.panel.tabs.iter().fold(row![], |bar, tab| {
            let selected = active == Some(tab.id.as_str());
            let label = button(text(tab.title.as_str()).size(14))
                .on_press(Message::SelectTab(tab.id.clone()))
                .style(style::tab(selected))
                .padding([0, 8]);
            let underline = container(space()).width(Fill).height(2);
            let underline = if selected { underline.style(style::tab_underline) } else { underline };
            bar.push(column![container(label).center_y(Fill), underline].width(Shrink).height(topbar::HEIGHT))
        });
        let scrollbar = scrollable::Scrollbar::new().width(2).scroller_width(2);
        let tabs = scrollable(tabs)
            .id(PANEL_TABS_ID)
            .direction(scrollable::Direction::Horizontal(scrollbar))
            .style(style::tab_scrollbar)
            .width(Fill);
        let tabs = mouse_area(tabs).on_scroll(Message::TabsWheel);
        let tab_bar = Element::from(container(tabs).style(style::chrome).padding([0, 4]).width(Fill)).map(map);

        let body = match active {
            Some(id) => panel_body(id),
            None => Element::from(
                container(text("Nothing to show").style(style::muted)).center_x(Fill).padding(24),
            )
            .map(map),
        };

        container(column![tab_bar, rule::horizontal(1).style(style::divider), container(body).width(Fill).height(Fill)])
            .id(PANEL_ID)
            .style(style::surface)
            .width(self.panel.width.min(self.max_panel_width()))
            .height(Fill)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> State {
        let (mut state, _) = State::new(Preference::System, PanelPrefs::default());
        state.set_tabs(vec![PanelTab::new("diff", "Diff"), PanelTab::new("files", "Files"), PanelTab::new("terminal", "Terminal")]);
        state
    }

    fn send(state: &mut State, message: Message) -> Option<Event> {
        state.update(message).1
    }

    fn toggle(state: &mut State) -> Option<Event> {
        send(state, Message::Topbar(topbar::Message::TogglePanel))
    }

    #[test]
    fn closing_and_reopening_the_panel_keeps_width_and_tab() {
        let mut state = frame();
        send(&mut state, Message::FrameSized(Size::new(1600.0, 900.0)));
        assert!(state.panel_open(), "the panel starts open");

        send(&mut state, Message::SelectTab("files".into()));
        send(&mut state, Message::DragStart);
        send(&mut state, Message::DragMove(1100.0));
        assert_eq!(send(&mut state, Message::DragEnd), Some(Event::PanelResized(497.0)));

        assert_eq!(toggle(&mut state), Some(Event::PanelToggled { open: false }));
        assert!(!state.panel_open());
        assert_eq!(toggle(&mut state), Some(Event::PanelToggled { open: true }));
        assert_eq!(state.panel_width(), 497.0);
        assert_eq!(state.active_tab(), Some("files"));
    }

    #[test]
    fn resizing_is_clamped_to_the_frame() {
        let mut state = frame();
        state.set_panel_open(true);
        send(&mut state, Message::FrameSized(Size::new(1200.0, 800.0)));
        send(&mut state, Message::DragStart);
        send(&mut state, Message::DragMove(10.0));
        assert_eq!(state.panel_width(), 1200.0 - SIDEBAR_WIDTH - MIN_CENTER_WIDTH - HANDLE_WIDTH);
        send(&mut state, Message::DragMove(1190.0));
        assert_eq!(state.panel_width(), MIN_PANEL_WIDTH);

        send(&mut state, Message::DragEnd);
        send(&mut state, Message::DragMove(600.0));
        assert_eq!(state.panel_width(), MIN_PANEL_WIDTH, "moves after release do not resize");
        assert_eq!(send(&mut state, Message::DragEnd), None);
    }

    #[test]
    fn removed_active_tab_falls_back_and_restored_tab_applies_when_added() {
        let (mut state, _) = State::new(
            Preference::Dark,
            PanelPrefs { open: true, width: 500.0, active: Some("agents".into()) },
        );
        state.add_tab(PanelTab::new("diff", "Diff"));
        assert_eq!(state.active_tab(), Some("diff"));
        state.add_tab(PanelTab::new("agents", "Agents"));
        assert_eq!(state.active_tab(), Some("agents"));

        state.remove_tab("agents");
        assert_eq!(state.active_tab(), Some("diff"));
        assert_eq!(state.update(Message::SelectTab("gone".into())).1, None);
        assert_eq!(state.active_tab(), Some("diff"));
    }

    #[test]
    fn show_tab_opens_the_panel_on_that_tab() {
        let mut state = frame();
        state.show_tab("terminal");
        assert!(state.panel_open());
        assert_eq!(state.active_tab(), Some("terminal"));
        assert_eq!(state.panel_prefs().active.as_deref(), Some("terminal"));
    }

    #[test]
    fn breadcrumb_follows_active_session_node_and_waiting_state() {
        let mut state = frame();
        state.set_nodes(vec![NodeStatus { key: "k-box".into(), name: "buildbox".into(), online: false, local: false }]);
        state.set_sessions([SessionSummary {
            id: "s1".into(),
            title: "Ship it".into(),
            project: Some("loa".into()),
            node: "k-box".into(),
            pinned: false,
            waiting: false,
            updated: 1,
        }]);
        let (_, event) = state.update(Message::Sidebar(sidebar::Message::OpenSession("s1".into())));
        assert_eq!(event, Some(Event::OpenSession("s1".into())));

        let crumb = state.topbar().breadcrumb().expect("breadcrumb");
        assert_eq!((crumb.project.as_deref(), crumb.title.as_str()), (Some("loa"), "Ship it"));
        assert_eq!((crumb.node.name.as_str(), crumb.node.online, crumb.waiting), ("buildbox", false, false));

        state.set_nodes(vec![NodeStatus { key: "k-box".into(), name: "buildbox".into(), online: true, local: false }]);
        let mut waiting = state.sidebar().session("s1").cloned().expect("session");
        waiting.waiting = true;
        state.upsert_session(waiting);
        let crumb = state.topbar().breadcrumb().expect("breadcrumb");
        assert!(crumb.node.online && crumb.waiting);

        state.remove_session("s1");
        assert!(state.topbar().breadcrumb().is_none());
    }

    #[test]
    fn theme_override_is_reported_and_applied() {
        let mut state = frame();
        send(&mut state, Message::SystemTheme(Mode::Light));
        assert!(!state.theme().extended_palette().is_dark);
        let (_, event) = state.update(Message::SetTheme(Preference::Dark));
        assert_eq!(event, Some(Event::SetTheme(Preference::Dark)));
        assert!(state.theme().extended_palette().is_dark);
    }
}
