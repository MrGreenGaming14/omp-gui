//! The app frame: sidebar on the left, top bar above a center slot, and a
//! toggleable right panel with a tab bar.
//!
//! The sidebar and the panel are resizable by their edges, and each of the
//! three panes can fill the window on its own. The panel remembers its width
//! and active tab while closed. Tabs are data: the app adds diff, files,
//! terminal, agents, session tree and surface tabs and draws the body of the
//! active one.

use crate::data::{NodeStatus, Nodes, SessionSummary, UsageMeter, short_key};
use crate::theme::{self, Appearance, ThemeRef};
use crate::{sidebar, topbar};
use iced::theme::Mode;
use iced::widget::operation::{self, AbsoluteOffset, RelativeOffset};
use iced::widget::{button, column, container, mouse_area, row, rule, scrollable, sensor};
use iced::{Alignment, Element, Fill, Size, Subscription, Task, Theme, mouse};
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, pad_x, sp};
use loa_ui_kit::widgets::{Tab, icon_button, label, splitter_edge, splitter_width, tab_strip};

/// Width of the sidebar before the user resizes it.
pub const SIDEBAR_WIDTH: f32 = 272.0;

/// Narrowest the sidebar gets.
pub const MIN_SIDEBAR_WIDTH: f32 = 200.0;

/// Widest the sidebar gets.
pub const MAX_SIDEBAR_WIDTH: f32 = 480.0;

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

/// Id of the project tab strip, which scrolls sideways when the tabs overflow.
pub const PROJECT_TABS_ID: &str = "loa-project-tabs";

/// Container id of the tab of `project` in the project tab strip.
pub fn project_tab_id(project: &str) -> String {
    format!("loa-project-tab-{project}")
}

/// Container id of the close button of the tab of `project`.
pub fn project_tab_close_id(project: &str) -> String {
    format!("loa-project-tab-close-{project}")
}

/// Height of the project tab strip.
const PROJECT_TABS_HEIGHT: f32 = 34.0;

/// Pixels a mouse wheel line scrolls the tab bar.
const WHEEL_LINE: f32 = 40.0;

/// Container id of the panel's pop-out button.
pub const PANEL_DETACH_ID: &str = "loa-panel-detach";

/// Container id of the panel's full screen button.
pub const PANEL_FULL_ID: &str = "loa-panel-full";

/// A pane that can fill the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The sidebar.
    Sidebar,
    /// The top bar and center.
    Center,
    /// The right panel.
    Panel,
}

/// Which edge is being dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drag {
    Sidebar,
    Panel,
}

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
}

/// Frame state: the sidebar, top bar, panel and theme.
#[derive(Debug, Clone)]
pub struct State {
    sidebar: sidebar::State,
    topbar: topbar::State,
    panel: Panel,
    theme: theme::State,
    frame: Option<Size>,
    /// Whether the project tab strip is drawn.
    project_tabs: bool,
    /// The projects with a tab, in order.
    open_projects: Vec<String>,
    active_project: Option<String>,
    sidebar_width: f32,
    dragging: Option<Drag>,
    full: Option<Pane>,
    /// Tabs shown in their own window instead of the panel.
    detached: Vec<String>,
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
    /// A project tab was clicked.
    OpenProjectTab(String),
    /// The close button of a project tab was clicked.
    CloseProjectTab(String),
    /// The mouse wheel turned over the project tab strip.
    ProjectTabsWheel(mouse::ScrollDelta),
    /// The panel edge was grabbed.
    DragStart,
    /// The sidebar edge was grabbed.
    SidebarDragStart,
    /// The cursor moved to this x while an edge is held.
    DragMove(f32),
    /// The held edge was released.
    DragEnd,
    /// A pane's full screen button was clicked.
    ToggleFull(Pane),
    /// The pop-out button of the active tab was clicked.
    Detach(String),
    /// "Show window" on a detached tab's placeholder.
    ShowDetached(String),
    /// "Reattach here" on a detached tab's placeholder.
    Reattach(String),
    /// The frame was laid out at this size.
    FrameSized(Size),
    /// The user picked a theme.
    SetTheme(ThemeRef),
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
    /// Drop this project's tab from the strip. Its sessions stay open.
    CloseProjectTab(String),
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
    /// The user finished resizing the sidebar to this width.
    SidebarResized(f32),
    /// Show this tab in its own window.
    DetachTab(String),
    /// Bring this detached tab's window to the front.
    ShowDetached(String),
    /// Put this detached tab back in the panel.
    ReattachTab(String),
    /// The user picked a theme. Persist it.
    SetTheme(ThemeRef),
    /// The active tokens changed to this generation; rebuild cached looks.
    ThemeChanged(u64),
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
            sidebar::Event::OpenUsage => Event::OpenUsage,
            sidebar::Event::OpenConfigs => Event::OpenConfigs,
            sidebar::Event::OpenSettings => Event::OpenSettings,
            sidebar::Event::SetPinned { id, pinned } => Event::SetPinned { id, pinned },
            sidebar::Event::SetProjectPinned { project, pinned } => Event::SetProjectPinned { project, pinned },
            sidebar::Event::DeleteSession(id) => Event::DeleteSession(id),
        }
    }
}

impl State {
    /// Builds the frame with the saved appearance and panel settings.
    /// The task asks the system for its theme.
    pub fn new(appearance: Appearance, panel: PanelPrefs) -> (Self, Task<Message>) {
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
            },
            theme: theme::State::new(appearance),
            frame: None,
            project_tabs: false,
            open_projects: Vec::new(),
            active_project: None,
            sidebar_width: SIDEBAR_WIDTH,
            dragging: None,
            full: None,
            detached: Vec::new(),
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

    /// The theme state: the picked theme, the registry's themes and the generation.
    pub fn theme_state(&self) -> &theme::State {
        &self.theme
    }

    /// Replaces the appearance (theme and overrides), e.g. after the config changed.
    pub fn set_appearance(&mut self, appearance: Appearance) -> Option<Event> {
        (self.theme.appearance() != &appearance).then(|| Event::ThemeChanged(self.theme.set_appearance(appearance)))
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
    pub fn set_pinned_projects(&mut self, pinned: impl IntoIterator<Item = String>) {
        self.sidebar.set_pinned_projects(pinned);
    }

    /// Highlights the project the user selected in the sidebar.
    /// The project tab strip highlights it too.
    pub fn set_active_project(&mut self, project: Option<String>) {
        self.sidebar.set_active_project(project.clone());
        self.active_project = project;
    }

    /// Shows or hides the project tab strip. Hidden, the frame is as it was
    /// without the strip.
    pub fn set_project_tabs(&mut self, enabled: bool) {
        self.project_tabs = enabled;
    }

    /// Whether the project tab strip is drawn.
    pub fn project_tabs(&self) -> bool {
        self.project_tabs
    }

    /// Replaces the projects with a tab, in order.
    pub fn set_open_projects(&mut self, projects: Vec<String>) {
        self.open_projects = projects;
    }

    /// The projects with a tab, in order.
    pub fn open_projects(&self) -> &[String] {
        &self.open_projects
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

    /// Sets the sidebar's usage meter.
    pub fn set_usage(&mut self, usage: Option<UsageMeter>) {
        self.sidebar.set_usage(usage);
    }

    /// The sidebar width the user chose.
    pub fn sidebar_width(&self) -> f32 {
        self.sidebar_width
    }

    /// Restores a saved sidebar width.
    pub fn set_sidebar_width(&mut self, width: f32) {
        self.sidebar_width = width.clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
    }

    /// The pane filling the window, if any.
    pub fn full(&self) -> Option<Pane> {
        self.full
    }

    /// Tells the panel which tabs show in their own window.
    pub fn set_detached(&mut self, tabs: Vec<String>) {
        self.detached = tabs;
    }

    /// Whether this tab shows in its own window.
    pub fn is_detached(&self, tab: &str) -> bool {
        self.detached.iter().any(|detached| detached == tab)
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

    /// Marks the project whose page fills the center (`None` when a session
    /// or the start screen does): the sidebar highlights it and the top bar
    /// names it.
    pub fn set_shown_project(&mut self, project: Option<String>) {
        self.sidebar.set_shown_project(project.clone());
        self.topbar.set_project_page(project);
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
        if self.dragging == Some(Drag::Panel) {
            self.dragging = None;
        }
        if !open && self.full == Some(Pane::Panel) {
            self.set_full(None);
        }
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
            (frame.width - self.sidebar_width - MIN_CENTER_WIDTH - 2.0 * splitter_width()).max(MIN_PANEL_WIDTH)
        })
    }

    fn set_full(&mut self, pane: Option<Pane>) {
        self.full = pane;
        self.topbar.set_full(pane == Some(Pane::Center));
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
                    topbar::Event::TogglePanel => {
                        self.set_panel_open(!self.panel.open);
                        Some(Event::PanelToggled { open: self.panel.open })
                    }
                    topbar::Event::ToggleFull => {
                        self.set_full(if self.full == Some(Pane::Center) { None } else { Some(Pane::Center) });
                        None
                    }
                    topbar::Event::OpenProject(project) => Some(Event::OpenProject(project)),
                });
                (task.map(Message::Topbar), event.flatten())
            }
            Message::SelectTab(id) => {
                let known = self.panel.tabs.iter().any(|tab| tab.id == id);
                let event = known.then(|| {
                    self.panel.active = Some(id.clone());
                    Event::SelectTab(id)
                });
                (Task::none(), event)
            }
            Message::TabsWheel(delta) => (wheel_tabs(PANEL_TABS_ID, delta), None),
            Message::ProjectTabsWheel(delta) => (wheel_tabs(PROJECT_TABS_ID, delta), None),
            Message::OpenProjectTab(project) => (Task::none(), Some(Event::OpenProject(project))),
            Message::CloseProjectTab(project) => (Task::none(), Some(Event::CloseProjectTab(project))),
            Message::DragStart => {
                self.dragging = self.panel.open.then_some(Drag::Panel);
                (Task::none(), None)
            }
            Message::SidebarDragStart => {
                self.dragging = Some(Drag::Sidebar);
                (Task::none(), None)
            }
            Message::DragMove(x) => {
                match self.dragging {
                    Some(Drag::Panel) => {
                        if let Some(frame) = self.frame {
                            let width = frame.width - x - splitter_width() / 2.0;
                            self.panel.width = width.min(self.max_panel_width()).max(MIN_PANEL_WIDTH);
                        }
                    }
                    Some(Drag::Sidebar) => {
                        self.sidebar_width = (x - splitter_width() / 2.0).clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH);
                    }
                    None => {}
                }
                (Task::none(), None)
            }
            Message::DragEnd => {
                let event = match self.dragging.take() {
                    Some(Drag::Panel) => Some(Event::PanelResized(self.panel.width)),
                    Some(Drag::Sidebar) => Some(Event::SidebarResized(self.sidebar_width)),
                    None => None,
                };
                (Task::none(), event)
            }
            Message::ToggleFull(pane) => {
                self.set_full(if self.full == Some(pane) { None } else { Some(pane) });
                (Task::none(), None)
            }
            Message::Detach(tab) => (Task::none(), Some(Event::DetachTab(tab))),
            Message::ShowDetached(tab) => (Task::none(), Some(Event::ShowDetached(tab))),
            Message::Reattach(tab) => (Task::none(), Some(Event::ReattachTab(tab))),
            Message::FrameSized(size) => {
                self.frame = Some(size);
                (Task::none(), None)
            }
            Message::SetTheme(theme) => {
                self.theme.set_preference(theme.clone());
                (Task::none(), Some(Event::SetTheme(theme)))
            }
            Message::SystemTheme(mode) => {
                let before = self.theme.variant();
                let generation = self.theme.set_system_mode(mode);
                (Task::none(), (self.theme.variant() != before).then_some(Event::ThemeChanged(generation)))
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
        project_files: Option<Element<'a, M>>,
        map: fn(Message) -> M,
    ) -> Element<'a, M> {
        let full = self.full;
        let show = |pane: Pane| full.is_none() || full == Some(pane);
        let mut frame = row![];

        if show(Pane::Sidebar) {
            let width = if full == Some(Pane::Sidebar) { iced::Length::Fill } else { iced::Length::Fixed(self.sidebar_width) };
            let sidebar = self.sidebar.view_with(move |message| map(Message::Sidebar(message)), project_files);
            frame = frame.push(container(sidebar).width(width).height(Fill));
        }
        if full.is_none() {
            frame = frame.push(Element::from(splitter_edge(Message::SidebarDragStart)).map(map));
        }
        if show(Pane::Center) {
            let topbar = self.topbar.view().map(Message::Topbar).map(map);
            let center = container(center).style(style::container(Surface::Ground)).width(Fill).height(Fill);
            let mut main = column![].width(Fill);
            if self.project_tabs {
                main = main.push(self.project_tabs_view(map)).push(rule::horizontal(1).style(style::rule));
            }
            frame = frame.push(main.push(topbar).push(center));
        }
        if self.panel.open && show(Pane::Panel) {
            if full.is_none() {
                frame = frame.push(Element::from(splitter_edge(Message::DragStart)).map(map));
            }
            frame = frame.push(self.panel_view(panel_body, map));
        }

        let mut tracked = mouse_area(frame);
        if self.dragging.is_some() {
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

    /// The strip of open projects above the top bar: a pill per project that
    /// opens it, with a close button that only drops the tab.
    fn project_tabs_view<'a, M: 'a>(&'a self, map: fn(Message) -> M) -> Element<'a, M> {
        let active = self.active_project.as_deref();
        let tabs = tab_strip(self.open_projects.iter().map(|project| {
            let close = button(tinted(Icon::X, T::Caption, Tone::Muted))
                .on_press(Message::CloseProjectTab(project.clone()))
                .style(style::button(ButtonKind::Ghost))
                .padding(sp(Sp::Xs) / 2.0);
            Tab {
                label: project.as_str(),
                icon: None,
                active: active == Some(project.as_str()),
                on: Message::OpenProjectTab(project.clone()),
                id: Some(project_tab_id(project)),
                trailing: Some(container(close).id(project_tab_close_id(project)).into()),
            }
        }));
        let tabs = scrollable(tabs).id(PROJECT_TABS_ID).direction(tab_scroll()).style(style::scrollbar(true)).width(Fill);
        let tabs = mouse_area(tabs).on_scroll(Message::ProjectTabsWheel);
        Element::from(container(tabs).style(style::container(Surface::Side)).padding(pad_x(Sp::S)).center_y(PROJECT_TABS_HEIGHT).width(Fill)).map(map)
    }

    fn panel_view<'a, M: 'a>(
        &'a self,
        panel_body: impl FnOnce(&'a str) -> Element<'a, M>,
        map: fn(Message) -> M,
    ) -> Element<'a, M> {
        let active = self.active_tab();
        let tabs = tab_strip(self.panel.tabs.iter().map(|tab| Tab {
            label: tab.title.as_str(),
            icon: tab_icon(&tab.id),
            active: active == Some(tab.id.as_str()),
            on: Message::SelectTab(tab.id.clone()),
            id: None,
            trailing: self.is_detached(&tab.id).then(|| tinted(Icon::ExternalLink, T::Caption, Tone::Muted).into()),
        }));
        // Embedded below the tabs, so it never covers a tab.
        let tabs = scrollable(tabs).id(PANEL_TABS_ID).direction(tab_scroll()).style(style::scrollbar(true)).width(Fill);
        let tabs = mouse_area(tabs).on_scroll(Message::TabsWheel);
        let is_full = self.full == Some(Pane::Panel);
        let full = icon_button(if is_full { Icon::Minimize } else { Icon::Maximize }, if is_full { "Restore panel" } else { "Fill window" }, Some(Message::ToggleFull(Pane::Panel)));
        let mut controls = row![tabs].spacing(sp(Sp::Xs)).align_y(Alignment::Center);
        if let Some(id) = active
            && !self.is_detached(id)
        {
            controls = controls.push(container(icon_button(Icon::ExternalLink, "Open in a window", Some(Message::Detach(id.to_owned())))).id(PANEL_DETACH_ID));
        }
        controls = controls.push(container(full).id(PANEL_FULL_ID));
        let tab_bar = Element::from(
            container(controls)
                .style(style::container(Surface::Side))
                .padding(pad_x(Sp::S))
                .center_y(topbar::HEIGHT)
                .width(Fill),
        )
        .map(map);

        let body = match active {
            Some(id) if self.is_detached(id) => {
                let title = self.panel.tabs.iter().find(|tab| tab.id == id).map_or(id, |tab| tab.title.as_str());
                let action = |s, kind, on| button(label(s, T::Label, Tone::Normal)).on_press(on).style(style::button(kind)).padding([sp(Sp::Xs) * 1.5, sp(Sp::M)]);
                Element::from(
                    container(
                        column![
                            tinted(Icon::ExternalLink, T::Title, Tone::Muted),
                            label(format!("{title} is in its own window"), T::Body, Tone::Normal),
                            row![
                                action("Show window", ButtonKind::Secondary, Message::ShowDetached(id.to_owned())),
                                action("Reattach here", ButtonKind::Primary, Message::Reattach(id.to_owned())),
                            ]
                            .spacing(sp(Sp::S)),
                        ]
                        .spacing(sp(Sp::M))
                        .align_x(Alignment::Center),
                    )
                    .center(Fill),
                )
                .map(map)
            }
            Some(id) => panel_body(id),
            None => Element::from(container(label("Nothing to show", T::Body, Tone::Muted)).center_x(Fill).padding(sp(Sp::Xl))).map(map),
        };

        let width = if is_full { iced::Length::Fill } else { iced::Length::Fixed(self.panel.width.min(self.max_panel_width())) };
        container(column![tab_bar, rule::horizontal(1).style(style::rule), container(body).width(Fill).height(Fill)])
            .id(PANEL_ID)
            .style(style::container(Surface::Side))
            .width(width)
            .height(Fill)
            .into()
    }
}

/// A sideways scrollbar under a tab strip.
fn tab_scroll() -> scrollable::Direction {
    let w = sp(Sp::Xs) / 2.0;
    scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(w).scroller_width(w).spacing(iced::Pixels::ZERO))
}

/// The icon of a known panel tab.
fn tab_icon(id: &str) -> Option<Icon> {
    match id {
        "diff" | "changes" => Some(Icon::GitCompare),
        "files" => Some(Icon::Folder),
        "terminal" => Some(Icon::Terminal),
        "agents" => Some(Icon::Bot),
        "tree" | "session-tree" => Some(Icon::GitFork),
        id if id.starts_with("surface") || id == "browser" => Some(Icon::Globe),
        _ => None,
    }
}

/// Scrolls the tab bar `id` for a wheel turn. The bar scrolls only sideways:
/// a vertical wheel turns it, sideways deltas reach the scrollable itself.
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
        let (mut state, _) = State::new(Appearance::default(), PanelPrefs::default());
        state.set_tabs(vec![PanelTab::new("diff", "Diff"), PanelTab::new("files", "Files"), PanelTab::new("terminal", "Terminal")]);
        state
    }

    fn send(state: &mut State, message: Message) -> Option<Event> {
        state.update(message).1
    }

    #[test]
    fn project_tab_messages_become_events_and_the_strip_is_off_by_default() {
        let mut state = frame();
        assert!(!state.project_tabs());
        state.set_open_projects(vec!["loa".into(), "docs".into()]);
        state.set_active_project(Some("docs".into()));
        assert_eq!(state.open_projects(), ["loa", "docs"]);
        assert_eq!(send(&mut state, Message::OpenProjectTab("loa".into())), Some(Event::OpenProject("loa".into())));
        assert_eq!(
            send(&mut state, Message::CloseProjectTab("docs".into())),
            Some(Event::CloseProjectTab("docs".into()))
        );
        assert_eq!(state.open_projects(), ["loa", "docs"], "the app owns the list");
        state.set_project_tabs(true);
        assert!(state.project_tabs());
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
        assert_eq!(state.panel_width(), 1200.0 - SIDEBAR_WIDTH - MIN_CENTER_WIDTH - 2.0 * splitter_width());
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
            Appearance { theme: ThemeRef("dark".into()), ..Default::default() },
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
        assert_eq!(state.theme_state().variant(), theme::Variant::Light);
        let dark = ThemeRef("dark".into());
        let (_, event) = state.update(Message::SetTheme(dark.clone()));
        assert_eq!(event, Some(Event::SetTheme(dark)));
        assert_eq!(state.theme_state().variant(), theme::Variant::Dark);
    }

    #[test]
    fn sidebar_resizes_within_bounds_and_reports_on_release() {
        let mut state = frame();
        send(&mut state, Message::FrameSized(Size::new(1600.0, 900.0)));
        send(&mut state, Message::SidebarDragStart);
        send(&mut state, Message::DragMove(343.0));
        assert_eq!(send(&mut state, Message::DragEnd), Some(Event::SidebarResized(340.0)));
        send(&mut state, Message::SidebarDragStart);
        send(&mut state, Message::DragMove(5000.0));
        assert_eq!(state.sidebar_width(), MAX_SIDEBAR_WIDTH);
        send(&mut state, Message::DragMove(10.0));
        assert_eq!(state.sidebar_width(), MIN_SIDEBAR_WIDTH);
    }

    #[test]
    fn panes_go_full_screen_one_at_a_time() {
        let mut state = frame();
        send(&mut state, Message::ToggleFull(Pane::Panel));
        assert_eq!(state.full(), Some(Pane::Panel));
        send(&mut state, Message::Topbar(topbar::Message::ToggleFull));
        assert_eq!(state.full(), Some(Pane::Center), "the chat takes over");
        send(&mut state, Message::Topbar(topbar::Message::ToggleFull));
        assert_eq!(state.full(), None);

        send(&mut state, Message::ToggleFull(Pane::Panel));
        toggle(&mut state);
        assert_eq!(state.full(), None, "closing the panel leaves its full screen");
    }

    #[test]
    fn detaching_a_tab_reports_it_and_reattaching_reports_back() {
        let mut state = frame();
        assert_eq!(send(&mut state, Message::Detach("terminal".into())), Some(Event::DetachTab("terminal".into())));
        state.set_detached(vec!["terminal".into()]);
        assert!(state.is_detached("terminal"));
        assert_eq!(send(&mut state, Message::ShowDetached("terminal".into())), Some(Event::ShowDetached("terminal".into())));
        assert_eq!(send(&mut state, Message::Reattach("terminal".into())), Some(Event::ReattachTab("terminal".into())));
    }
}
