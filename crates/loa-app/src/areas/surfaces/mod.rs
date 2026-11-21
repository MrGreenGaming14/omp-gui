//! The Surfaces panel tab: browser tabs, documents, VMs and surfaces MCP
//! servers declare, drawn by the generic surface renderer, and the
//! `surface_*` tools sessions drive them with.

use std::collections::HashMap;
use std::sync::Arc;

use iced::{Element, Subscription, Task};
use loa_contracts::harness::SessionConfig;
use loa_bus::SessionId;
use loa_core::{NodeId, PatchSink, TurnStatus, attr};
use loa_contracts::surface::SurfaceUri;
use loa_surfaces::{CaptureMode, HubConfig, HubEvent, Listed, McpEndpoint, Opened, SurfaceHub, SurfaceTools, dom};
use loa_ui_surface::{Options, Panel, panel};
use loa_view::surface::{Capture, Intent};

use crate::adapters::surface::{DocsRenderer, decoders};
use crate::view::surface::{available, browser_status, capture_from_view, capture_status};
use crate::area::{perform, Area, AreaCommand, AreaMessage, Ctx, HostToolSet, PanelTab, Target};
use crate::watch::{PtrKey, watch};
use loa_runtime::session::{SessionEvent, dom_turns};

/// Id of the Surfaces panel tab.
pub const TAB: &str = "surfaces";

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
    Encoders(Arc<loa_surfaces::LiveVideo>),
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
    /// The area over `hub`; starts the MCP endpoint and reads the surfaces
    /// the MCP servers declare.
    pub(crate) fn with_hub(hub: Arc<SurfaceHub>, ctx: &mut Ctx) -> (Self, Task<Message>) {
        let tools = SurfaceTools::new(Arc::clone(&hub));
        for spec in tools.host_tools().0 {
            ctx.set_app_tool(loa_view::panel::ToolInfo::from(&spec));
        }
        let options = Options::new(decoders(), Arc::new(DocsRenderer::new(hub.config().fonts.clone())));
        let mut panel = Panel::new(options, browser_status(&hub), ctx.runtime.cwd().to_path_buf());
        #[cfg(feature = "remote-video")]
        let encoders = video::start(&hub, &mut panel);
        #[cfg(not(feature = "remote-video"))]
        let encoders = Task::none();
        panel.set_capture(capture_status(&hub, false));
        let state = Self {
            hub,
            endpoint: None,
            mcp: ctx.services.node.mcp.clone(),
            panel,
            recorded: HashMap::new(),
            unrecorded: Vec::new(),
            tools: Arc::clone(&tools),
        };
        let endpoint = perform(
            async move { McpEndpoint::start(tools).await.map(|endpoint| Endpoint(Arc::new(endpoint))) },
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
        if let Err(error) = self.hub.set_capture(capture_from_view(capture)) {
            ctx.notify(error.to_string());
            self.panel.set_capture(capture_status(&self.hub, false));
            return Task::none();
        }
        save(ctx, |settings| {
            settings.browser_capture = match capture {
                Capture::Screencast => loa_settings::app::BrowserCapture::Screencast,
                Capture::VirtualDisplay => loa_settings::app::BrowserCapture::VirtualDisplay,
            };
        });
        self.list()
    }

    fn refresh_declared(&self) -> Task<Message> {
        let hub = Arc::clone(&self.hub);
        let mcp = self.mcp.clone();
        Task::perform(async move { hub.refresh_declared(&mcp).await }, Message::Declared)
    }

    fn panel_event(&mut self, intent: Intent, ctx: &mut Ctx) -> Task<Message> {
        let hub = Arc::clone(&self.hub);
        match intent {
            Intent::OpenBrowser(url) => {
                perform(async move { hub.open_browser(&url).await }, Message::Opened)
            }
            Intent::OpenDocument(path) => {
                perform(async move { hub.open_document(&path).await }, Message::Opened)
            }
            Intent::OpenVm { address, password } => {
                perform(async move { hub.open_vm(&address, password).await }, Message::Opened)
            }
            Intent::Open(uri) => {
                perform(async move { hub.open(&uri).await }, Message::Opened)
            }
            Intent::Close(uri) => {
                perform(async move { hub.close(&uri).await }, Message::Closed)
            }
            #[cfg(feature = "remote-video")]
            Intent::SetVideo(enabled) => video::set(&self.hub, enabled, ctx),
            // The launcher has no live video toggle without the feature.
            #[cfg(not(feature = "remote-video"))]
            Intent::SetVideo(_) => Task::none(),
            Intent::SetCapture(capture) => self.set_capture(capture, ctx),
            Intent::EncodingUnsupported { uri, encoding, supported } => {
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
        let Some(session) = ctx.runtime.sessions().active() else {
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
            let Some(session) = ctx.runtime.sessions().get(session.as_str()) else { continue };
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

/// Changes the `[surfaces]` table of the central config.
fn save(ctx: &mut Ctx, change: impl FnOnce(&mut loa_settings::app::SurfaceSettings)) {
    ctx.edit_config("surface settings", |config| change(&mut config.surfaces));
}

impl AreaMessage for Message {
    type Area = State;
}

impl Area for State {
    const ID: &'static str = "surfaces";
    const COMMANDS: &'static [AreaCommand<Self>] = &[
        AreaCommand::<Self>::new("surfaces.browser", "Open browser surface", "Surfaces", |state, _, ctx| state.launcher(ctx)),
        AreaCommand::<Self>::new("surfaces.document", "Open document surface", "Surfaces", |state, _, ctx| state.launcher(ctx)),
        AreaCommand::<Self>::new("surfaces.vm", "Open VNC surface", "Surfaces", |state, _, ctx| state.launcher(ctx)),
    ];

    type Message = Message;

    fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let mut config = HubConfig::discover(&ctx.services.storage.dirs.data.join("surfaces"));
        if let Some(settings) = ctx.read_config("surface settings").map(|config| config.surfaces) {
            #[cfg(feature = "remote-video")]
            video::configure(&mut config, &settings);
            if settings.browser_capture == loa_settings::app::BrowserCapture::VirtualDisplay && config.virtual_display.is_ok() {
                config.capture = CaptureMode::VirtualDisplay;
            }
        }
        Self::with_hub(SurfaceHub::new(config), ctx)
    }

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
                let _ = ctx.report("Closing the surface failed", result);
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
                    .map(available)
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

    fn panel_tabs(&self) -> Vec<PanelTab> {
        vec![PanelTab::new(TAB, "Surfaces", false).icon(loa_view::theme::Icon::Globe)]
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

    /// The hub's events, a resync after falling behind; and the changes of
    /// the app's MCP servers, which re-read the declared surfaces.
    fn subscription(&self) -> Subscription<Message> {
        let (hub, mcp) = (Arc::clone(&self.hub), self.mcp.clone());
        Subscription::batch([
            watch(PtrKey(&self.hub), move || hub.events(), |event| Some(event.map_or(Message::Resync, Message::Hub))),
            watch("surfaces.mcp", move || mcp.events(), |event| match event {
                Ok(loa_mcp::Event::Status { status: loa_mcp::Status::Starting, .. } | loa_mcp::Event::ToolsChanged { .. }) => None,
                Ok(_) | Err(_) => Some(Message::Mcp),
            }),
        ])
    }
}

#[cfg(test)]
mod tests;
#[cfg(feature = "remote-video")]
mod video;
