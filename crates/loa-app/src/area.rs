//! The contract every feature area implements, and the context the app hands
//! each one.
//!
//! An area lives in `src/areas/<area>/` and owns its state, its `Message`
//! type and its behavior. The app holds every area in one
//! [`Registry`](crate::areas::Registry), built from the `AREAS` table, and
//! calls it through [`AreaDyn`]:
//!
//! - its own messages travel as [`Message::Area`] (build them with
//!   [`Message::area`]) and reach [`Area::update`]
//! - its [`Area::COMMANDS`] are listed in the palette, run by their ids and,
//!   with a `slash` name, typed as `/` commands in the composer
//! - panel tabs it declares ([`Area::panel_tabs`]) are drawn by
//!   [`Area::view_tab`] and opened with [`Ctx::open_tab`]
//! - a full-center screen is drawn by [`Area::view_screen`] while
//!   [`Ctx::show_screen`] points at the area
//! - every new or resumed session gets [`Area::contribute`] and
//!   [`Area::host_tools`]
//! - every session change arrives at [`Area::on_session_event`]

use std::any::Any;
use std::fmt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use iced::{Element, Subscription, Task};
use loa_contracts::harness::{Capabilities, HarnessSession, HostToolSpec, HostTools, Prompt, SessionConfig};
use loa_bus::SessionId;
use loa_ui_session::composer as composer;
use loa_ui_shell::palette;
use loa_ui_shell as shell;

use crate::app::Message;
use crate::services::Services;
pub use crate::session::{Session, SessionEvent, Sessions};

/// A message of one area; [`AreaMessage::Area`] handles it.
pub trait AreaMessage: Clone + fmt::Debug + Send + 'static {
    type Area: Area<Message = Self>;
}

/// Any area's message, behind [`Message::Area`].
pub struct AreaMsg(Box<dyn Erased>);

trait Erased: Any + Send + fmt::Debug {
    fn clone_box(&self) -> Box<dyn Erased>;
}

impl<M: AreaMessage> Erased for M {
    fn clone_box(&self) -> Box<dyn Erased> {
        Box::new(self.clone())
    }
}

impl AreaMsg {
    pub fn new<M: AreaMessage>(message: M) -> Self {
        Self(Box::new(message))
    }

    /// The message as `M`, or itself back when it is another type.
    pub fn downcast<M: AreaMessage>(self) -> Result<M, Self> {
        if !(&*self.0 as &dyn Any).is::<M>() {
            return Err(self);
        }
        let any: Box<dyn Any> = self.0;
        Ok(*any.downcast::<M>().unwrap_or_else(|_| unreachable!("checked above")))
    }

    pub fn downcast_ref<M: AreaMessage>(&self) -> Option<&M> {
        (&*self.0 as &dyn Any).downcast_ref::<M>()
    }
}

impl Clone for AreaMsg {
    fn clone(&self) -> Self {
        Self(self.0.clone_box())
    }
}

impl fmt::Debug for AreaMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Message {
    /// `message` for the area that handles its type.
    pub fn area<M: AreaMessage>(message: M) -> Self {
        Message::Area(M::Area::ID, AreaMsg::new(message))
    }
}

/// A right panel tab an area draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelTab {
    /// Unique across the app; the built-in tabs are `diff`, `files` and
    /// `terminal`.
    pub id: String,
    pub title: String,
    pub icon: Option<loa_ui_kit::Icon>,
    /// Listed in the panel from the start. Other tabs are added when opened
    /// (palette "Open … tab" or [`Ctx::open_tab`]).
    pub shown: bool,
}

impl PanelTab {
    pub fn new(id: impl Into<String>, title: impl Into<String>, shown: bool) -> Self {
        Self { id: id.into(), title: title.into(), icon: None, shown }
    }

    /// The tab of a kit pane, named and drawn as the pane says.
    pub fn of<P: loa_ui_kit::widgets::PanelPane>(pane: &P, shown: bool) -> Self {
        Self { icon: Some(pane.icon()), ..Self::new(pane.id(), pane.title(), shown) }
    }

    pub fn icon(self, icon: loa_ui_kit::Icon) -> Self {
        Self { icon: Some(icon), ..self }
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
    /// Make `project` and its folder `cwd` the active ones: the Files tab,
    /// new terminals, the Diff tab and new sessions follow. Running sessions
    /// keep their folder.
    ActivateProject { project: String, cwd: PathBuf },
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
    pub app_tools: Vec<loa_ui_panel::tools::ToolInfo>,
    /// Peers the peers area knows; refreshed by it.
    pub peers: Vec<loa_p2p::PeerInfo>,
    /// The running peer node, set by the peers area each time it opens one;
    /// clones of the slot see the current node. Areas add protocols to it
    /// before it starts (`loa_p2p::Node::add_protocol`).
    pub node: loa_p2p::NodeSlot,
    /// The app's `/` commands, from every [`Area::COMMANDS`].
    pub slash: Vec<composer::Command>,
    pub(crate) screen: Option<&'static str>,
    pub(crate) notice: Option<String>,
    pub(crate) requests: Vec<Request>,
    pub(crate) events: Vec<SessionEvent>,
}

impl Ctx {
    /// Shows a one line notice under the center content.
    pub fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
    }

    /// The value of `result`; on failure a `{what}: {error}` notice and
    /// `None`.
    pub fn report<T>(&mut self, what: &str, result: Result<T, impl fmt::Display>) -> Option<T> {
        result.inspect_err(|error| self.notify(format!("{what}: {error}"))).ok()
    }

    /// The central config; on failure a `Can't read the {what}` notice and `None`.
    pub fn read_config(&mut self, what: &str) -> Option<loa_config::Config> {
        self.services.config.load().inspect_err(|error| self.notify(format!("Can't read the {what}: {error}"))).ok()
    }

    /// Edits the central config; on failure a `Can't save the {what}` notice and `None`.
    pub fn edit_config<R>(&mut self, what: &str, change: impl FnOnce(&mut loa_config::Config) -> R) -> Option<R> {
        self.services.config.edit(change).inspect_err(|error| self.notify(format!("Can't save the {what}: {error}"))).ok()
    }

    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Lists `info` among the app tools, replacing the entry of the same
    /// name.
    pub fn set_app_tool(&mut self, info: loa_ui_panel::tools::ToolInfo) {
        match self.app_tools.iter_mut().find(|tool| tool.name == info.name) {
            Some(tool) => *tool = info,
            None => self.app_tools.push(info),
        }
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

    /// Replaces the chat with the [`Area::view_screen`] of area `area`
    /// ([`Area::ID`]) until [`Ctx::close_screen`], Esc, or another session
    /// is opened.
    pub fn show_screen(&mut self, area: &'static str) {
        self.screen = Some(area);
    }

    pub fn close_screen(&mut self) {
        self.screen = None;
    }

    /// The area whose screen fills the center.
    pub fn screen(&self) -> Option<&'static str> {
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
            self.shell.sidebar_mut().remove_session(id);
        }
        if was_active {
            self.shell.sidebar_mut().set_active(None);
            self.shell.set_session_status(None);
            self.events.push(SessionEvent::Activated(None));
        }
        self.events.push(SessionEvent::Closed(session.id().clone()));
        session.harness().cloned()
    }
}

/// A feature area. See the module docs for how the app drives it.
pub trait Area: Sized + 'static {
    /// The area's directory name: the prefix of its command ids and the key
    /// of its messages and screen.
    const ID: &'static str;

    /// The area's commands, in palette order. An id is `<ID>.<name>`.
    const COMMANDS: &'static [AreaCommand<Self>] = &[];

    type Message: AreaMessage<Area = Self>;

    fn new(ctx: &mut Ctx) -> (Self, Task<Self::Message>);

    fn update(&mut self, message: Self::Message, ctx: &mut Ctx) -> Task<Self::Message>;

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

    /// Called for every area after `project` became the active project and
    /// `cwd` the active folder ([`Request::ActivateProject`]).
    fn project_activated(&mut self, project: &str, cwd: &Path, ctx: &mut Ctx) -> Task<Self::Message> {
        let _ = (project, cwd, ctx);
        Task::none()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::none()
    }
}

/// When a command shows in the palette.
pub enum Listed<S> {
    Always,
    /// While a session is active.
    Session,
    /// Run by id, key or `/` name only.
    Never,
    /// Under the title returned, when there is one.
    When(fn(&S, &Ctx) -> Option<&'static str>),
    /// Once per `(argument, title)`; the command's id ends in `.` and the
    /// argument follows it.
    Each(fn(&S, &Ctx) -> Vec<(String, String)>),
}

/// One command: listed in the palette, run by its id (an id ending in `.`
/// takes the rest as its argument) and, with a `slash` name, typed as
/// `/<name> <argument>` in the composer.
pub struct CommandSpec<S, Run> {
    pub id: &'static str,
    pub title: &'static str,
    pub section: &'static str,
    /// The `/` name and its description in the composer's list.
    pub slash: Option<(&'static str, &'static str)>,
    pub listed: Listed<S>,
    pub run: Run,
}

/// An area's command: runs on the area with the argument.
pub type AreaCommand<A> = CommandSpec<A, fn(&mut A, &str, &mut Ctx) -> Task<<A as Area>::Message>>;

impl<A: Area> AreaCommand<A> {
    /// A command listed under `title` in `section`, run by `run`.
    pub const fn new(id: &'static str, title: &'static str, section: &'static str, run: fn(&mut A, &str, &mut Ctx) -> Task<A::Message>) -> Self {
        Self { id, title, section, slash: None, listed: Listed::Always, run }
    }
}

impl<S, Run: Copy> CommandSpec<S, Run> {
    pub const fn listed(self, listed: Listed<S>) -> Self {
        Self { listed, ..self }
    }

    /// Also typed as `/<name>` in the composer, listed with `description`.
    pub const fn slash(self, name: &'static str, description: &'static str) -> Self {
        Self { slash: Some((name, description)), ..self }
    }
}

impl<S, Run> CommandSpec<S, Run> {
    /// The argument when `id` names this command.
    pub fn matches<'a>(&self, id: &'a str) -> Option<&'a str> {
        if self.id.ends_with('.') { id.strip_prefix(self.id) } else { (id == self.id).then_some("") }
    }

    /// The palette rows of this command.
    pub fn palette(&self, state: &S, ctx: &Ctx) -> Vec<palette::Command> {
        let row = |id, title: &str| palette::Command::new(id, title, self.section);
        let fixed = |title| vec![row(palette::CommandId::from_static(self.id), title)];
        match self.listed {
            Listed::Always => fixed(self.title),
            Listed::Session if ctx.sessions.active().is_some() => fixed(self.title),
            Listed::Session | Listed::Never => Vec::new(),
            Listed::When(title) => title(state, ctx).map(fixed).unwrap_or_default(),
            Listed::Each(each) => {
                each(state, ctx).into_iter().map(|(arg, title)| row(palette::CommandId::new(format!("{}{arg}", self.id)), &title)).collect()
            }
        }
    }
}

/// The command of `specs` that `id` names, with its argument: the one with
/// exactly this id, else the first whose prefix it starts with.
pub fn find_command<'a, S, Run>(specs: &'a [CommandSpec<S, Run>], id: &'a str) -> Option<(&'a CommandSpec<S, Run>, &'a str)> {
    let exact = specs.iter().find(|spec| spec.id == id).map(|spec| (spec, ""));
    exact.or_else(|| specs.iter().find_map(|spec| spec.matches(id).map(|arg| (spec, arg))))
}

/// The first of `specs` whose `/` name `text` types, with its arguments.
pub fn find_slash<'a, S, Run>(specs: &'a [CommandSpec<S, Run>], text: &'a str) -> Option<(&'a CommandSpec<S, Run>, &'a str)> {
    let (name, args) = split_slash(text)?;
    specs.iter().find(|spec| spec.slash.is_some_and(|(slash, _)| slash == name)).map(|spec| (spec, args))
}

/// A `/` command typed in the composer: its name and its trimmed arguments.
pub fn split_slash(text: &str) -> Option<(&str, &str)> {
    let rest = text.trim().strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    Some((name, args.trim()))
}

/// The composer rows of the `/` commands among `specs`.
pub fn slash_rows<S, Run>(specs: &[CommandSpec<S, Run>]) -> impl Iterator<Item = composer::Command> + '_ {
    specs.iter().filter_map(|spec| spec.slash).map(|(name, description)| composer::Command { name: name.to_owned(), description: description.to_owned() })
}

/// Runs `future` off the UI thread and hands its result, the error as text,
/// to `done`.
pub fn perform<T, E, M>(
    future: impl Future<Output = Result<T, E>> + Send + 'static,
    done: impl FnOnce(Result<T, String>) -> M + Send + 'static,
) -> Task<M>
where
    T: Send + 'static,
    E: fmt::Display,
    M: Send + 'static,
{
    Task::perform(async move { future.await.map_err(|error| error.to_string()) }, done)
}

/// An area as the app holds it: every [`Area`] call with its messages
/// erased to [`Message`].
pub trait AreaDyn: Any {
    fn id(&self) -> &'static str;
    fn update(&mut self, message: AreaMsg, ctx: &mut Ctx) -> Task<Message>;
    fn commands(&self, ctx: &Ctx) -> Vec<palette::Command>;
    /// Runs the command `id` names; `None` when it names none of this area's.
    fn run(&mut self, id: &str, ctx: &mut Ctx) -> Option<Task<Message>>;
    /// Runs the `/` command `text` types; `None` when it isn't this area's.
    fn slash(&mut self, text: &str, ctx: &mut Ctx) -> Option<Task<Message>>;
    fn slash_rows(&self) -> Vec<composer::Command>;
    fn panel_tabs(&self) -> Vec<PanelTab>;
    fn view_tab<'a>(&'a self, id: &str, ctx: &'a Ctx) -> Option<Element<'a, Message>>;
    fn view_screen<'a>(&'a self, ctx: &'a Ctx) -> Option<Element<'a, Message>>;
    fn contribute(&self, config: &mut SessionConfig, target: &Target<'_>, ctx: &Ctx);
    fn host_tools(&self, config: &SessionConfig, target: &Target<'_>, ctx: &Ctx) -> Option<HostToolSet>;
    fn before_send(&mut self, session: &SessionId, prompt: &mut Prompt, ctx: &mut Ctx);
    fn on_session_event(&mut self, event: &SessionEvent, ctx: &mut Ctx) -> Task<Message>;
    fn project_activated(&mut self, project: &str, cwd: &Path, ctx: &mut Ctx) -> Task<Message>;
    fn subscription(&self) -> Subscription<Message>;
}

/// [`AreaDyn`] over an [`Area`].
pub struct Typed<A>(pub A);

impl<A: Area> AreaDyn for Typed<A> {
    fn id(&self) -> &'static str {
        A::ID
    }

    fn update(&mut self, message: AreaMsg, ctx: &mut Ctx) -> Task<Message> {
        match message.downcast::<A::Message>() {
            Ok(message) => self.0.update(message, ctx).map(Message::area),
            Err(other) => {
                ctx.notify(format!("The {} area can't handle {other:?}", A::ID));
                Task::none()
            }
        }
    }

    fn commands(&self, ctx: &Ctx) -> Vec<palette::Command> {
        A::COMMANDS.iter().flat_map(|spec| spec.palette(&self.0, ctx)).collect()
    }

    fn run(&mut self, id: &str, ctx: &mut Ctx) -> Option<Task<Message>> {
        let (spec, arg) = find_command(A::COMMANDS, id)?;
        Some((spec.run)(&mut self.0, arg, ctx).map(Message::area))
    }

    fn slash(&mut self, text: &str, ctx: &mut Ctx) -> Option<Task<Message>> {
        let (spec, args) = find_slash(A::COMMANDS, text)?;
        Some((spec.run)(&mut self.0, args, ctx).map(Message::area))
    }

    fn slash_rows(&self) -> Vec<composer::Command> {
        slash_rows(A::COMMANDS).collect()
    }

    fn panel_tabs(&self) -> Vec<PanelTab> {
        self.0.panel_tabs()
    }

    fn view_tab<'a>(&'a self, id: &str, ctx: &'a Ctx) -> Option<Element<'a, Message>> {
        self.0.view_tab(id, ctx).map(|view| view.map(Message::area))
    }

    fn view_screen<'a>(&'a self, ctx: &'a Ctx) -> Option<Element<'a, Message>> {
        self.0.view_screen(ctx).map(|view| view.map(Message::area))
    }

    fn contribute(&self, config: &mut SessionConfig, target: &Target<'_>, ctx: &Ctx) {
        self.0.contribute(config, target, ctx);
    }

    fn host_tools(&self, config: &SessionConfig, target: &Target<'_>, ctx: &Ctx) -> Option<HostToolSet> {
        self.0.host_tools(config, target, ctx)
    }

    fn before_send(&mut self, session: &SessionId, prompt: &mut Prompt, ctx: &mut Ctx) {
        self.0.before_send(session, prompt, ctx);
    }

    fn on_session_event(&mut self, event: &SessionEvent, ctx: &mut Ctx) -> Task<Message> {
        self.0.on_session_event(event, ctx).map(Message::area)
    }

    fn project_activated(&mut self, project: &str, cwd: &Path, ctx: &mut Ctx) -> Task<Message> {
        self.0.project_activated(project, cwd, ctx).map(Message::area)
    }

    fn subscription(&self) -> Subscription<Message> {
        self.0.subscription().map(Message::area)
    }
}
