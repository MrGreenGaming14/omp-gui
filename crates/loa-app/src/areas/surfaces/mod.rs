//! The Surfaces panel tab: browser tabs, documents, VMs and surfaces MCP
//! servers declare, drawn by the generic surface renderer, and the
//! `surface_*` tools sessions drive them with.
//!
//! - The [`SurfaceHub`] opens and keeps the surfaces, whoever opened them
//!   (the launcher in the tab, or an agent tool call).
//! - Sessions get the tools as host tools when their harness takes them,
//!   else as the loopback [`McpEndpoint`]'s MCP servers.
//! - Every surface opened is recorded as a `<surface>` element in the active
//!   session, under its running turn, and marked closed when it closes.

use std::collections::HashMap;
use std::sync::Arc;

use iced::futures::{Stream, stream};
use iced::{Element, Subscription, Task};
use loa_contracts::harness::SessionConfig;
use loa_bus::SessionId;
use loa_core::{NodeId, PatchSink, TurnStatus, attr};
use loa_contracts::surface::SurfaceUri;
use loa_surface_hub::{CaptureMode, HubConfig, HubEvent, Listed, McpEndpoint, Opened, SurfaceHub, SurfaceTools, dom};
use loa_ui_shell::palette;
use loa_ui_surface::{Available, BrowserStatus, Capture, CaptureStatus, DocsRenderers, Panel, PanelOptions, panel};
use tokio::sync::broadcast::error::RecvError;

use crate::area::{Area, AreaId, Ctx, HostToolSet, PanelTab, SessionEvent, Target};
use crate::session::dom_turns;

/// Id of the Surfaces panel tab.
pub const TAB: &str = "surfaces";

const OPEN_BROWSER: &str = "browser";
const OPEN_DOCUMENT: &str = "document";
const OPEN_VM: &str = "vm";

/// The running loopback MCP endpoint, shared by the sessions that use it.
#[derive(Clone)]
pub struct Endpoint(Arc<McpEndpoint>);

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Endpoint").field(&self.0.url("")).finish()
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Panel(panel::Message),
    /// Opening a surface from the launcher finished.
    Opened(Result<Opened, String>),
    /// Closing a surface finished.
    Closed(Result<(), String>),
    /// Something changed in the hub.
    Hub(HubEvent),
    /// The hub's events were missed: take its state again.
    Resync,
    /// The hub's surface list, for the launcher.
    Listed(Vec<Listed>),
    /// The MCP servers changed: read the surfaces they declare again.
    Mcp,
    /// Reading declared surfaces finished; the servers whose listing failed.
    Declared(Vec<(String, String)>),
    /// The loopback MCP endpoint started.
    Endpoint(Result<Endpoint, String>),
    /// The machine's video encoders were tried.
    #[cfg(feature = "remote-video")]
    Encoders(Arc<loa_surface_hub::LiveVideo>),
    /// Whether the running browser keeps the other capture until its tabs
    /// close.
    CapturePending(bool),
}

pub struct State {
    hub: Arc<SurfaceHub>,
    tools: Arc<SurfaceTools>,
    endpoint: Option<Endpoint>,
    mcp: loa_mcp::Manager,
    panel: Panel,
    /// The `<surface>` elements recorded for each surface: session and node.
    recorded: HashMap<SurfaceUri, Vec<(SessionId, NodeId)>>,
    /// Surfaces opened while no session was shown; recorded in the next one
    /// shown.
    unrecorded: Vec<SurfaceUri>,
}

impl State {
    pub fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let mut config = HubConfig::discover(&ctx.services.dirs.data.join("surfaces"));
        if let Some(settings) = ctx.read_config("surface settings").map(|config| config.surfaces) {
            #[cfg(feature = "remote-video")]
            video::configure(&mut config, &settings);
            if settings.browser_capture == loa_config::BrowserCapture::VirtualDisplay && config.virtual_display.is_ok() {
                config.capture = CaptureMode::VirtualDisplay;
            }
        }
        Self::with_hub(SurfaceHub::new(config), ctx)
    }

    /// The area over `hub`; starts the MCP endpoint and reads the surfaces
    /// the MCP servers declare.
    pub(crate) fn with_hub(hub: Arc<SurfaceHub>, ctx: &mut Ctx) -> (Self, Task<Message>) {
        let tools = SurfaceTools::new(Arc::clone(&hub));
        for spec in tools.host_tools().0 {
            ctx.set_app_tool(loa_ui_tools::ToolInfo::from(&spec));
        }
        let documents = DocsRenderers::new(hub.config().fonts.clone());
        #[allow(unused_mut)]
        let mut options = PanelOptions { documents: Some(Arc::new(documents)), ..PanelOptions::default() };
        #[cfg(feature = "remote-video")]
        video::options(&mut options);
        let mut panel = Panel::new(options, browser_status(&hub), ctx.services.cwd.clone());
        #[cfg(feature = "remote-video")]
        let encoders = video::start(&hub, &mut panel);
        #[cfg(not(feature = "remote-video"))]
        let encoders = Task::none();
        panel.set_capture(capture_status(&hub, false));
        let state = Self {
            hub,
            endpoint: None,
            mcp: ctx.services.mcp.clone(),
            panel,
            recorded: HashMap::new(),
            unrecorded: Vec::new(),
            tools: Arc::clone(&tools),
        };
        let endpoint = Task::perform(
            async move { McpEndpoint::start(tools).await.map(|endpoint| Endpoint(Arc::new(endpoint))).map_err(|e| e.to_string()) },
            Message::Endpoint,
        );
        let task = Task::batch([endpoint, state.refresh_declared(), state.list(), encoders]);
        (state, task)
    }

    pub fn hub(&self) -> &Arc<SurfaceHub> {
        &self.hub
    }

    pub fn panel(&self) -> &Panel {
        &self.panel
    }

    /// The hub's surface list and whether the browser's capture is pending.
    fn list(&self) -> Task<Message> {
        let hub = Arc::clone(&self.hub);
        let pending = Arc::clone(&self.hub);
        Task::batch([
            Task::perform(async move { hub.list().await }, Message::Listed),
            Task::perform(async move { pending.capture_pending().await }, Message::CapturePending),
        ])
    }

    fn set_capture(&mut self, capture: Capture, ctx: &mut Ctx) -> Task<Message> {
        let mode = match capture {
            Capture::Screencast => CaptureMode::Screencast,
            Capture::VirtualDisplay => CaptureMode::VirtualDisplay,
        };
        if let Err(error) = self.hub.set_capture(mode) {
            ctx.notify(error.to_string());
            self.panel.set_capture(capture_status(&self.hub, false));
            return Task::none();
        }
        save(ctx, |settings| {
            settings.browser_capture = match mode {
                CaptureMode::Screencast => loa_config::BrowserCapture::Screencast,
                CaptureMode::VirtualDisplay => loa_config::BrowserCapture::VirtualDisplay,
            };
        });
        self.list()
    }

    fn refresh_declared(&self) -> Task<Message> {
        let hub = Arc::clone(&self.hub);
        let mcp = self.mcp.clone();
        Task::perform(async move { hub.refresh_declared(&mcp).await }, Message::Declared)
    }

    fn panel_event(&mut self, event: panel::Event, ctx: &mut Ctx) -> Task<Message> {
        let hub = Arc::clone(&self.hub);
        match event {
            panel::Event::OpenBrowser(url) => {
                Task::perform(async move { hub.open_browser(&url).await.map_err(|e| e.to_string()) }, Message::Opened)
            }
            panel::Event::OpenDocument(path) => {
                Task::perform(async move { hub.open_document(&path).await.map_err(|e| e.to_string()) }, Message::Opened)
            }
            panel::Event::OpenVm { address, password } => {
                Task::perform(async move { hub.open_vm(&address, password).await.map_err(|e| e.to_string()) }, Message::Opened)
            }
            panel::Event::Open(uri) => {
                Task::perform(async move { hub.open(&uri).await.map_err(|e| e.to_string()) }, Message::Opened)
            }
            panel::Event::Close(uri) => {
                Task::perform(async move { hub.close(&uri).await.map_err(|e| e.to_string()) }, Message::Closed)
            }
            #[cfg(feature = "remote-video")]
            panel::Event::SetVideo(enabled) => video::set(&self.hub, enabled, ctx),
            panel::Event::SetCapture(capture) => self.set_capture(capture, ctx),
            panel::Event::EncodingUnsupported { uri, encoding, supported } => {
                let supported: Vec<String> = supported.iter().map(|encoding| format!("{encoding:?}")).collect();
                ctx.notify(format!(
                    "{uri} streams {encoding:?}, which the renderer can't show (it shows {})",
                    supported.join(", ")
                ));
                Task::none()
            }
        }
    }

    /// Shows a surface the hub opened (by the launcher or an agent) in its
    /// tab and records it in the active session.
    fn show(&mut self, opened: &Opened, ctx: &mut Ctx) -> Task<Message> {
        let attach = self.panel.attach(Arc::clone(&opened.surface), opened.origin.server()).map(Message::Panel);
        if !self.recorded.contains_key(&opened.info.uri) {
            self.record(opened, ctx);
        }
        attach
    }

    /// Records `<surface>` for `opened` in the active session, under its
    /// running turn, or keeps it for the next session shown.
    fn record(&mut self, opened: &Opened, ctx: &mut Ctx) {
        let Some(session) = ctx.sessions.active() else {
            if !self.unrecorded.contains(&opened.info.uri) {
                self.unrecorded.push(opened.info.uri.clone());
            }
            return;
        };
        let Some(root) = session.dom().root().map(|root| root.id) else { return };
        let running = dom_turns(session.dom())
            .last()
            .filter(|turn| {
                matches!(turn.str_attr(attr::STATUS).and_then(TurnStatus::parse), Some(TurnStatus::Running | TurnStatus::Waiting))
            })
            .map(|turn| turn.id);
        let draft = dom::opened(running.unwrap_or(root), &opened.info, &opened.origin);
        match session.writer().commit(draft) {
            Ok(patch) => {
                let id = session.id().clone();
                self.recorded.entry(opened.info.uri.clone()).or_default().push((id, patch.node));
            }
            Err(error) => ctx.notify(format!("Can't record {} in the session: {error}", opened.info.uri)),
        }
    }

    /// Marks the `<surface>` elements of `uri` closed in the sessions still
    /// open.
    fn record_closed(&mut self, uri: &SurfaceUri, ctx: &mut Ctx) {
        self.unrecorded.retain(|open| open != uri);
        for (session, node) in self.recorded.remove(uri).unwrap_or_default() {
            let Some(session) = ctx.sessions.get(session.as_str()) else { continue };
            if let Err(error) = session.writer().commit(dom::closed(node)) {
                ctx.notify(format!("Can't record {uri} closed in the session: {error}"));
            }
        }
    }

    fn launcher(&mut self, ctx: &mut Ctx) -> Task<Message> {
        ctx.open_tab(TAB);
        self.panel.update(panel::Message::Launcher).0.map(Message::Panel)
    }
}

/// What the launcher says about the browser.
fn browser_status(hub: &SurfaceHub) -> BrowserStatus {
    match hub.browser_executable() {
        Some(executable) => BrowserStatus::Ready { executable: executable.to_owned(), sandboxed: hub.browser_sandboxed() },
        None => BrowserStatus::Missing,
    }
}

/// What the launcher says about the browser capture.
fn capture_status(hub: &SurfaceHub, pending: bool) -> CaptureStatus {
    CaptureStatus {
        selected: match hub.capture() {
            CaptureMode::Screencast => Capture::Screencast,
            CaptureMode::VirtualDisplay => Capture::VirtualDisplay,
        },
        virtual_display_missing: hub.config().virtual_display.as_ref().err().cloned(),
        pending,
    }
}

/// Changes the `[surfaces]` table of the central config.
fn save(ctx: &mut Ctx, change: impl FnOnce(&mut loa_config::SurfaceSettings)) {
    ctx.edit_config("surface settings", |config| change(&mut config.surfaces));
}

impl Area for State {
    type Message = Message;

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Panel(message) => {
                let (task, event) = self.panel.update(message);
                let action = event.map_or_else(Task::none, |event| self.panel_event(event, ctx));
                Task::batch([task.map(Message::Panel), action])
            }
            Message::Opened(Ok(opened)) => Task::batch([self.show(&opened, ctx), self.list()]),
            Message::Opened(Err(error)) => {
                self.panel.failed(error);
                Task::none()
            }
            Message::Closed(result) => {
                if let Err(error) = result {
                    ctx.notify(format!("Closing the surface failed: {error}"));
                }
                self.list()
            }
            Message::Hub(HubEvent::Opened { info, .. }) => {
                let Some(opened) = self.hub.opened(&info.uri) else { return Task::none() };
                if self.panel.surface(&info.uri).is_none() {
                    // Opened by an agent: show it.
                    ctx.open_tab(TAB);
                }
                Task::batch([self.show(&opened, ctx), self.list()])
            }
            Message::Hub(HubEvent::Closed { uri }) => {
                self.panel.detach(&uri);
                self.record_closed(&uri, ctx);
                self.list()
            }
            Message::Hub(HubEvent::Declared) => self.list(),
            Message::Resync => {
                let open = self.hub.open_surfaces();
                let gone: Vec<SurfaceUri> = self
                    .panel
                    .open_surfaces()
                    .map(|info| info.uri.clone())
                    .filter(|uri| !open.iter().any(|opened| &opened.info.uri == uri))
                    .collect();
                for uri in gone {
                    self.panel.detach(&uri);
                    self.record_closed(&uri, ctx);
                }
                let shown: Vec<_> = open.iter().map(|opened| self.show(opened, ctx)).collect();
                Task::batch(shown.into_iter().chain([self.list()]))
            }
            Message::Listed(listed) => {
                let available = listed
                    .into_iter()
                    .filter(|listed| !listed.open)
                    .map(|listed| Available { server: listed.origin.server().to_owned(), info: listed.info })
                    .collect();
                self.panel.set_available(available);
                Task::none()
            }
            Message::Mcp => self.refresh_declared(),
            Message::Declared(failed) => {
                if !failed.is_empty() {
                    let failed: Vec<String> = failed.iter().map(|(server, error)| format!("{server}: {error}")).collect();
                    ctx.notify(format!("Can't list the surfaces of MCP servers ({})", failed.join("; ")));
                }
                Task::none()
            }
            Message::Endpoint(Ok(endpoint)) => {
                self.endpoint = Some(endpoint);
                Task::none()
            }
            Message::Endpoint(Err(error)) => {
                ctx.notify(format!("The surface MCP servers didn't start: {error}"));
                Task::none()
            }
            #[cfg(feature = "remote-video")]
            Message::Encoders(encoders) => {
                self.panel.set_video(video::status(self.panel.video().enabled, &encoders));
                Task::none()
            }
            Message::CapturePending(pending) => {
                self.panel.set_capture(capture_status(&self.hub, pending));
                Task::none()
            }
        }
    }

    fn palette_commands(&self, _: &Ctx) -> Vec<palette::Command> {
        [(OPEN_BROWSER, "Open browser surface"), (OPEN_DOCUMENT, "Open document surface"), (OPEN_VM, "Open VNC surface")]
            .into_iter()
            .map(|(name, label)| palette::Command::new(AreaId::Surfaces.command(name), label, "Surfaces"))
            .collect()
    }

    fn run_command(&mut self, id: &str, ctx: &mut Ctx) -> Option<Task<Message>> {
        let name = id.strip_prefix(AreaId::Surfaces.key())?.strip_prefix('.')?;
        [OPEN_BROWSER, OPEN_DOCUMENT, OPEN_VM].contains(&name).then(|| self.launcher(ctx))
    }

    fn panel_tabs(&self) -> Vec<PanelTab> {
        vec![PanelTab::new(TAB, "Surfaces", false).icon(loa_ui_kit::Icon::Globe)]
    }

    fn view_tab<'a>(&'a self, id: &str, _: &'a Ctx) -> Option<Element<'a, Message>> {
        (id == TAB).then(|| self.panel.view().map(Message::Panel))
    }

    fn contribute(&self, config: &mut SessionConfig, target: &Target<'_>, _: &Ctx) {
        if target.capabilities.host_tools || !target.capabilities.mcp_servers {
            return;
        }
        if let Some(endpoint) = &self.endpoint {
            config.mcp_servers.extend(endpoint.0.specs());
        }
    }

    fn host_tools(&self, _: &SessionConfig, target: &Target<'_>, _: &Ctx) -> Option<HostToolSet> {
        target.capabilities.host_tools.then(|| {
            let (specs, handler) = self.tools.host_tools();
            HostToolSet { specs, handler }
        })
    }

    fn on_session_event(&mut self, event: &SessionEvent, ctx: &mut Ctx) -> Task<Message> {
        match event {
            SessionEvent::Activated(Some(_)) => {
                for uri in std::mem::take(&mut self.unrecorded) {
                    if let Some(opened) = self.hub.opened(&uri) {
                        self.record(&opened, ctx);
                    }
                }
            }
            SessionEvent::Closed(id) => {
                for nodes in self.recorded.values_mut() {
                    nodes.retain(|(session, _)| session != id);
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run_with(HubWatch(Arc::clone(&self.hub)), hub_events),
            Subscription::run_with(McpWatch(self.mcp.clone()), mcp_events),
        ])
    }
}

/// The hub whose events the subscription streams; one per app.
struct HubWatch(Arc<SurfaceHub>);

impl std::hash::Hash for HubWatch {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

fn hub_events(watch: &HubWatch) -> impl Stream<Item = Message> + use<> {
    stream::unfold(watch.0.events(), |mut events| async move {
        let message = match events.recv().await {
            Ok(event) => Message::Hub(event),
            Err(RecvError::Lagged(_)) => Message::Resync,
            Err(RecvError::Closed) => return None,
        };
        Some((message, events))
    })
}

/// The app's MCP servers, whose changes re-read the declared surfaces.
struct McpWatch(loa_mcp::Manager);

impl std::hash::Hash for McpWatch {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        "surfaces.mcp".hash(state);
    }
}

fn mcp_events(watch: &McpWatch) -> impl Stream<Item = Message> + use<> {
    stream::unfold(watch.0.events(), |mut events| async move {
        loop {
            match events.recv().await {
                Ok(loa_mcp::Event::Status { status: loa_mcp::Status::Starting, .. } | loa_mcp::Event::ToolsChanged { .. }) => {}
                Ok(_) | Err(RecvError::Lagged(_)) => return Some((Message::Mcp, events)),
                Err(RecvError::Closed) => return None,
            }
        }
    })
}

#[cfg(test)]
mod tests;
#[cfg(feature = "remote-video")]
mod video;
