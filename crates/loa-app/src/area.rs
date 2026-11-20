//! The contract every feature area implements, and the context the app hands
//! each one.
//!
//! An area lives in `src/areas/<area>/` and owns its state, its `Message`
//! type and its behavior. The app holds one field and one `Message` variant
//! per area and calls it through [`Area`]:
//!
//! - its own messages reach [`Area::update`]
//! - palette commands it lists ([`Area::palette_commands`]) run through
//!   [`Area::run_command`]; ids must start with [`AreaId::key`] and a dot
//!   (build them with [`AreaId::command`])
//! - panel tabs it declares ([`Area::panel_tabs`]) are drawn by
//!   [`Area::view_tab`] and opened with [`Ctx::open_tab`]
//! - a full-center screen is drawn by [`Area::view_screen`] while
//!   [`Ctx::show_screen`] points at the area
//! - every new or resumed session gets [`Area::contribute`] and
//!   [`Area::host_tools`]
//! - every session change arrives at [`Area::on_session_event`]

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use iced::{Element, Subscription, Task};
use loa_adapter::{Capabilities, HarnessSession, HostToolSpec, HostTools, Prompt, SessionConfig};
use loa_bus::SessionId;
use loa_ui_composer as composer;
use loa_ui_palette as palette;
use loa_ui_shell as shell;

use crate::services::Services;
pub use crate::session::{Session, SessionEvent, Sessions};

/// The areas, in the order the app calls them.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AreaId {
    Harness,
    Sessions,
    Decisions,
    Tools,
    Surfaces,
    Models,
    Projects,
    Peers,
    Updater,
    Sandbox,
    Agents,
}

impl AreaId {
    pub const ALL: [AreaId; 11] = [
        AreaId::Harness,
        AreaId::Sessions,
        AreaId::Decisions,
        AreaId::Tools,
        AreaId::Surfaces,
        AreaId::Models,
        AreaId::Projects,
        AreaId::Peers,
        AreaId::Updater,
        AreaId::Sandbox,
        AreaId::Agents,
    ];

    /// The area's directory name and the prefix of its command ids.
    pub const fn key(self) -> &'static str {
        match self {
            AreaId::Harness => "harness",
            AreaId::Sessions => "sessions",
            AreaId::Decisions => "decisions",
            AreaId::Tools => "tools",
            AreaId::Surfaces => "surfaces",
            AreaId::Models => "models",
            AreaId::Projects => "projects",
            AreaId::Peers => "peers",
            AreaId::Updater => "updater",
            AreaId::Sandbox => "sandbox",
            AreaId::Agents => "agents",
        }
    }

    /// The palette command id `<key>.<name>`.
    pub fn command(self, name: &str) -> palette::CommandId {
        palette::CommandId::new(format!("{}.{name}", self.key()))
    }

    /// The area a command id belongs to, by its prefix.
    pub fn of_command(id: &str) -> Option<Self> {
        let (prefix, _) = id.split_once('.')?;
        Self::ALL.into_iter().find(|area| area.key() == prefix)
    }
}

/// A right panel tab an area draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelTab {
    /// Unique across the app; the built-in tabs are `diff`, `files` and
    /// `terminal`.
    pub id: String,
    pub title: String,
    /// Listed in the panel from the start. Other tabs are added when opened
    /// (palette "Open … tab" or [`Ctx::open_tab`]).
    pub shown: bool,
}

impl PanelTab {
    pub fn new(id: impl Into<String>, title: impl Into<String>, shown: bool) -> Self {
        Self { id: id.into(), title: title.into(), shown }
    }
}

/// The harness a session config is being built for.
pub struct Target<'a> {
    /// Harness id, e.g. `omp`.
    pub harness: &'a str,
    pub capabilities: &'a Capabilities,
    /// `true` when the session is resumed rather than started.
    pub resume: bool,
}

/// Tools an area serves to sessions through the harness's host tool
/// mechanism: the specs and the handler they route to.
pub struct HostToolSet {
    pub specs: Vec<HostToolSpec>,
    pub handler: Arc<dyn HostTools>,
}

/// App-level actions an area asks for. Run by the app right after the area
/// call returns.
#[derive(Clone)]
pub enum Request {
    /// Start a session on `Sessions::harness`, with every area's
    /// contribution.
    NewSession,
    /// Show this session (loading it from its journal when needed).
    OpenSession(String),
    /// Show this session in its own window, without making it active.
    DetachSession(String),
    /// Send a prompt to the active session (resuming it when needed).
    Prompt(Prompt),
    /// Add the panel tab with this id when it isn't listed, and show it.
    OpenTab(String),
    /// Add the panel tab with this id when it isn't listed, without taking
    /// the panel from the tab it shows; a closed panel opens on it.
    RevealTab(String),
    /// Start a session the area creates in the library itself (fork,
    /// rewind, compaction), with every area's contribution; it becomes the
    /// active session.
    Spawn(Spawn),
    /// Make `project` the active one, and its default folder `cwd`, when it
    /// has one, the active folder: the Files tab, new terminals, the Diff tab
    /// and new sessions follow. Running sessions keep their folder.
    ActivateProject { project: String, cwd: Option<PathBuf> },
    /// `project` was deleted: drop its tab and pin, and stop it being the
    /// active project.
    ForgetProject(String),
    /// Run this message through the app's `update`, e.g. to hand a decision
    /// answer to the area that owns it.
    Forward(Box<crate::app::Message>),
}

/// Starts a session whose journal the closure creates: it gets the session's
/// full config (every area's contribution, keyed by [`Spawn::id`]) and
/// returns the running harness.
pub type SpawnStart = Arc<
    dyn Fn(SessionConfig) -> Pin<Box<dyn Future<Output = Result<Arc<dyn HarnessSession>, String>> + Send>> + Send + Sync,
>;

/// A session [`Request::Spawn`] starts.
#[derive(Clone)]
pub struct Spawn {
    /// Id of the new session; `start` must create it in the library.
    pub id: SessionId,
    /// Harness it runs on.
    pub harness: String,
    pub start: SpawnStart,
    /// Sent once it runs.
    pub prompt: Option<Prompt>,
}

/// What every area may read and change: the services, the sessions and the
/// shared UI (shell frame, composer, keymap), plus app requests.
pub struct Ctx {
    pub services: Services,
    pub sessions: Sessions,
    pub shell: shell::State,
    pub composer: composer::State,
    pub keymap: palette::Keymap,
    /// App tools areas serve to sessions, as the Tools panel lists them.
    /// Each area replaces its own entries by name ([`Ctx::set_app_tool`]).
    pub app_tools: Vec<loa_ui_tools::ToolInfo>,
    /// Bumped whenever an app tool is added or changed. Views that list
    /// the tools compare it to know when to rebuild.
    app_tools_rev: u64,
    /// The running peer node, set by the peers area each time it opens one;
    /// clones of the slot see the current node. Areas add protocols to it
    /// before it starts (`loa_node::Node::add_protocol`).
    pub node: loa_node::NodeSlot,
    pub(crate) screen: Option<AreaId>,
    pub(crate) notice: Option<String>,
    pub(crate) requests: Vec<Request>,
    pub(crate) events: Vec<SessionEvent>,
}

impl Ctx {
    /// Starts the context with no app tools, notice or requests.
    pub(crate) fn new(
        services: Services,
        sessions: Sessions,
        shell: shell::State,
        composer: composer::State,
        keymap: palette::Keymap,
    ) -> Self {
        Self {
            services,
            sessions,
            shell,
            composer,
            keymap,
            app_tools: Vec::new(),
            app_tools_rev: 0,
            node: loa_node::NodeSlot::default(),
            screen: None,
            notice: None,
            requests: Vec::new(),
            events: Vec::new(),
        }
    }

    /// Shows a one line notice under the center content.
    pub fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
    }

    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Lists `info` among the app tools, replacing the entry of the same
    /// name. An unchanged entry leaves the revision alone.
    pub fn set_app_tool(&mut self, info: loa_ui_tools::ToolInfo) {
        match self.app_tools.iter_mut().find(|tool| tool.name == info.name) {
            Some(tool) if *tool == info => return,
            Some(tool) => *tool = info,
            None => self.app_tools.push(info),
        }
        self.app_tools_rev += 1;
    }

    /// Changes whenever `app_tools` does.
    pub fn app_tools_rev(&self) -> u64 {
        self.app_tools_rev
    }

    pub fn request(&mut self, request: Request) {
        self.requests.push(request);
    }

    /// Adds the panel tab `id` when it isn't listed, opens the panel and
    /// shows the tab.
    pub fn open_tab(&mut self, id: impl Into<String>) {
        self.requests.push(Request::OpenTab(id.into()));
    }

    /// Lists the panel tab `id` because something relevant to it just
    /// started, without taking the panel from the tab the user reads; a
    /// closed panel opens on it.
    pub fn reveal_tab(&mut self, id: impl Into<String>) {
        self.requests.push(Request::RevealTab(id.into()));
    }

    /// Replaces the chat with `area`'s [`Area::view_screen`] until
    /// [`Ctx::close_screen`], Esc, or another session is opened.
    pub fn show_screen(&mut self, area: AreaId) {
        self.screen = Some(area);
    }

    pub fn close_screen(&mut self) {
        self.screen = None;
    }

    /// The area whose screen fills the center.
    pub fn screen(&self) -> Option<AreaId> {
        self.screen
    }

    /// Points the composer's harness picker and new sessions at the active
    /// session's harness.
    pub fn show_harness(&mut self) {
        if let Some(id) = self.sessions.active().and_then(Session::harness_id) {
            self.sessions.harness = Some(id.to_owned());
        }
        self.composer.set_harnesses(self.services.harnesses.options(), self.sessions.harness.as_deref());
    }

    /// Closes session `id`: its writer stops and its journal is released; it
    /// stays in Recents and reopens from the journal. Returns its harness
    /// for the caller to shut down.
    pub fn close_session(&mut self, id: &str) -> Option<Arc<dyn HarnessSession>> {
        self.end_session(id, false)
    }

    /// Closes session `id`, deletes its journal and removes it from the
    /// sidebar. Returns its harness for the caller to shut down.
    pub fn discard_session(&mut self, id: &str) -> Option<Arc<dyn HarnessSession>> {
        self.end_session(id, true)
    }

    fn end_session(&mut self, id: &str, delete: bool) -> Option<Arc<dyn HarnessSession>> {
        let was_active = self.sessions.is_active(id);
        let (session, error) = self.sessions.close(&self.services, id, delete);
        if let Some(error) = error {
            self.notify(error);
        }
        let session = session?;
        if delete {
            self.shell.remove_session(id);
        }
        if was_active {
            self.shell.set_active_session(None);
            self.shell.set_session_status(None);
            self.events.push(SessionEvent::Activated(None));
        }
        self.events.push(SessionEvent::Closed(session.id().clone()));
        session.harness().cloned()
    }
}

/// A feature area. See the module docs for how the app drives it.
pub trait Area {
    type Message: Clone + std::fmt::Debug + Send + 'static;

    fn update(&mut self, message: Self::Message, ctx: &mut Ctx) -> Task<Self::Message>;

    /// Commands listed in the palette. Collected each time the palette opens.
    fn palette_commands(&self, ctx: &Ctx) -> Vec<palette::Command> {
        let _ = ctx;
        Vec::new()
    }

    /// Runs one of this area's commands; `None` when `id` isn't one.
    fn run_command(&mut self, id: &str, ctx: &mut Ctx) -> Option<Task<Self::Message>> {
        let _ = (id, ctx);
        None
    }

    /// The panel tabs this area draws.
    fn panel_tabs(&self) -> Vec<PanelTab> {
        Vec::new()
    }

    /// Draws tab `id`; `None` when it isn't this area's.
    fn view_tab<'a>(&'a self, id: &str, ctx: &'a Ctx) -> Option<Element<'a, Self::Message>> {
        let _ = (id, ctx);
        None
    }

    /// The full-center screen, drawn while [`Ctx::screen`] names this area.
    fn view_screen<'a>(&'a self, ctx: &'a Ctx) -> Option<Element<'a, Self::Message>> {
        let _ = ctx;
        None
    }

    /// Adds to the config of every session started or resumed on `target`:
    /// MCP servers, context, disabled tools, env. Host tools go through
    /// [`Area::host_tools`]; the app sets `host_tools` and
    /// `host_tool_handler` after every area contributed.
    fn contribute(&self, config: &mut SessionConfig, target: &Target<'_>, ctx: &Ctx) {
        let _ = (config, target, ctx);
    }

    /// Host tools for a session on `target`. `config` is the session's
    /// config after every area's `contribute`. The app routes every area's
    /// tools through one `HostToolRouter` by tool name.
    fn host_tools(&self, config: &SessionConfig, target: &Target<'_>, ctx: &Ctx) -> Option<HostToolSet> {
        let _ = (config, target, ctx);
        None
    }

    /// Called for every area right before `prompt` goes to the running
    /// harness of session `session`; areas may rewrite it.
    fn before_send(&mut self, session: &SessionId, prompt: &mut Prompt, ctx: &mut Ctx) {
        let _ = (session, prompt, ctx);
    }

    /// Called for every change of every open session.
    fn on_session_event(&mut self, event: &SessionEvent, ctx: &mut Ctx) -> Task<Self::Message> {
        let _ = (event, ctx);
        Task::none()
    }

    /// Called for every area after `project` became the active project;
    /// `cwd` is the active folder ([`Request::ActivateProject`]).
    fn project_activated(&mut self, project: &str, cwd: &Path, ctx: &mut Ctx) -> Task<Self::Message> {
        let _ = (project, cwd, ctx);
        Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::none()
    }
}
