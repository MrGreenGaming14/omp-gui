//! Running registered servers.

use loa_util::LockExt;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use loa_surface::SurfaceUri;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation, JsonObject,
    ReadResourceRequestParams, Resource, ResourceContents, Tool,
};
use rmcp::service::NotificationContext;
use rmcp::{ClientHandler, Peer, RoleClient};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::connect::{self, Connection};
use crate::spec::ServerSpec;
use crate::{Error, Result, ToolFilter};

/// Events buffered per subscriber before the oldest are dropped.
const EVENT_CAPACITY: usize = 256;

/// Delay between restarts of a server that keeps failing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    /// Wait before the first restart.
    pub initial: Duration,
    /// The wait doubles per failure up to this. A connection that stayed up
    /// this long resets it.
    pub max: Duration,
}

impl Backoff {
    fn delay(&self, attempt: u32) -> Duration {
        self.initial.saturating_mul(1 << attempt.min(16)).min(self.max)
    }
}

/// How the manager runs servers.
#[derive(Clone, Debug)]
pub struct Options {
    pub backoff: Backoff,
    /// Limit for spawning or reaching a server and finishing the handshake.
    pub connect_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            backoff: Backoff { initial: Duration::from_millis(500), max: Duration::from_secs(30) },
            connect_timeout: Duration::from_secs(30),
        }
    }
}

/// What the Tools panel shows next to a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Spawning or connecting, at first or after a failure.
    Starting,
    Running,
    /// Disabled, stopped by the user, or shut down.
    Stopped,
    /// The last start failed or the server went away. A restart is pending
    /// unless the server is stopped.
    Error(String),
}

/// Something the UI should redraw for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Status { server: String, status: Status },
    /// The server's tool list changed; read it again with [`Manager::tools`].
    ToolsChanged { server: String },
    /// The server's resources (surfaces among them) changed.
    ResourcesChanged { server: String },
    Removed { server: String },
}

/// One server as the Tools panel lists it.
#[derive(Clone, Debug)]
pub struct ServerView {
    pub spec: ServerSpec,
    pub status: Status,
    /// Its tools, never mixed with another server's.
    pub tools: Vec<ToolView>,
}

#[derive(Clone, Debug)]
pub struct ToolView {
    pub tool: Tool,
    /// What the filter passed to [`Manager::view`] says.
    pub allowed: bool,
}

/// The `surface://` uri a resource declares, if it is a surface.
pub fn surface_uri(resource: &Resource) -> Option<SurfaceUri> {
    resource.uri.parse().ok()
}

/// Runs the registered MCP servers. Cloning shares the same servers.
#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

struct Inner {
    servers: Mutex<BTreeMap<String, Entry>>,
    events: broadcast::Sender<Event>,
    options: Options,
    http: reqwest::Client,
}

struct Entry {
    spec: ServerSpec,
    shared: Arc<Shared>,
    run: Option<Run>,
}

/// A supervisor task keeping one server connected.
struct Run {
    stop: CancellationToken,
    task: JoinHandle<()>,
}

/// State a supervisor publishes and calls read.
struct Shared {
    name: String,
    status: watch::Sender<Status>,
    peer: Mutex<Option<Peer<RoleClient>>>,
    tools: Mutex<Arc<[Tool]>>,
    events: broadcast::Sender<Event>,
}


impl Shared {
    fn set_status(&self, status: Status) {
        self.status.send_replace(status.clone());
        let _ = self.events.send(Event::Status { server: self.name.clone(), status });
    }

    fn set_tools(&self, tools: Vec<Tool>) {
        *self.tools.locked() = tools.into();
    }

    fn peer(&self) -> Result<Peer<RoleClient>> {
        self.peer.locked().clone().ok_or_else(|| Error::NotRunning(self.name.clone()))
    }
}

/// The client side handler of one connection: keeps the tool list fresh.
struct Client {
    shared: Arc<Shared>,
}

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("loa", env!("CARGO_PKG_VERSION")))
    }

    async fn on_tool_list_changed(&self, context: NotificationContext<RoleClient>) {
        match context.peer.list_all_tools().await {
            Ok(tools) => {
                self.shared.set_tools(tools);
                let _ = self.shared.events.send(Event::ToolsChanged { server: self.shared.name.clone() });
            }
            Err(e) => tracing::warn!(server = self.shared.name, "relisting tools: {e}"),
        }
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<RoleClient>) {
        let _ = self.shared.events.send(Event::ResourcesChanged { server: self.shared.name.clone() });
    }
}

impl Manager {
    pub fn new(options: Options) -> Result<Self> {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let inner = Inner { servers: Mutex::default(), events, options, http: connect::http_client()? };
        Ok(Self { inner: Arc::new(inner) })
    }

    /// Status changes, tool and resource list changes of every server.
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    /// Registers a server and starts it when it is enabled.
    pub fn add(&self, spec: ServerSpec) -> Result<()> {
        spec.validate()?;
        let mut servers = self.inner.servers.locked();
        if servers.contains_key(&spec.name) {
            return Err(Error::NameTaken(spec.name));
        }
        let shared = Arc::new(Shared {
            name: spec.name.clone(),
            status: watch::Sender::new(Status::Stopped),
            peer: Mutex::default(),
            tools: Mutex::new(Arc::new([])),
            events: self.inner.events.clone(),
        });
        let mut entry = Entry { spec, shared, run: None };
        if entry.spec.enabled {
            entry.run = Some(self.spawn(&entry));
        }
        servers.insert(entry.spec.name.clone(), entry);
        Ok(())
    }

    /// Stops a server and forgets it.
    pub async fn remove(&self, name: &str) -> Result<ServerSpec> {
        let entry = self.inner.servers.locked().remove(name).ok_or_else(|| Error::UnknownServer(name.to_owned()))?;
        if let Some(run) = entry.run {
            finish(run).await;
        }
        let _ = self.inner.events.send(Event::Removed { server: name.to_owned() });
        Ok(entry.spec)
    }

    /// Swaps the server registered as `name` for `spec`, which may carry a
    /// new name. The old one is stopped first.
    pub async fn replace(&self, name: &str, spec: ServerSpec) -> Result<()> {
        spec.validate()?;
        if spec.name != name && self.inner.servers.locked().contains_key(&spec.name) {
            return Err(Error::NameTaken(spec.name));
        }
        self.remove(name).await?;
        self.add(spec)
    }

    /// Starts a stopped server (and marks it enabled in the running set).
    pub fn start(&self, name: &str) -> Result<()> {
        let mut servers = self.inner.servers.locked();
        let entry = servers.get_mut(name).ok_or_else(|| Error::UnknownServer(name.to_owned()))?;
        entry.spec.enabled = true;
        if entry.run.as_ref().is_none_or(|run| run.task.is_finished()) {
            entry.run = Some(self.spawn(entry));
        }
        Ok(())
    }

    /// Stops a server and waits until its child has exited.
    pub async fn stop(&self, name: &str) -> Result<()> {
        let run = {
            let mut servers = self.inner.servers.locked();
            let entry = servers.get_mut(name).ok_or_else(|| Error::UnknownServer(name.to_owned()))?;
            entry.spec.enabled = false;
            entry.run.take()
        };
        if let Some(run) = run {
            finish(run).await;
        }
        Ok(())
    }

    /// Stops and starts a server now, skipping any pending backoff.
    pub async fn restart(&self, name: &str) -> Result<()> {
        self.stop(name).await?;
        self.start(name)
    }

    /// Stops every server and waits for their children to exit.
    pub async fn shutdown(&self) {
        let runs: Vec<Run> = self.inner.servers.locked().values_mut().filter_map(|entry| entry.run.take()).collect();
        for run in &runs {
            run.stop.cancel();
        }
        for run in runs {
            let _ = run.task.await;
        }
    }

    /// The registered specs, sorted by name.
    pub fn specs(&self) -> Vec<ServerSpec> {
        self.inner.servers.locked().values().map(|entry| entry.spec.clone()).collect()
    }

    pub fn status(&self, name: &str) -> Option<Status> {
        self.inner.servers.locked().get(name).map(|entry| entry.shared.status.borrow().clone())
    }

    /// Follows one server's status.
    pub fn watch(&self, name: &str) -> Option<watch::Receiver<Status>> {
        self.inner.servers.locked().get(name).map(|entry| entry.shared.status.subscribe())
    }

    /// Every server with its status and its tools, each marked with what
    /// `filter` allows.
    pub fn view(&self, filter: &dyn ToolFilter) -> Vec<ServerView> {
        self.inner.servers.locked()
            .values()
            .map(|entry| {
                let tools = entry.shared.tools.locked().clone();
                let tools = tools
                    .iter()
                    .map(|tool| ToolView { allowed: filter.allows(&entry.spec.name, &tool.name), tool: tool.clone() })
                    .collect();
                ServerView { spec: entry.spec.clone(), status: entry.shared.status.borrow().clone(), tools }
            })
            .collect()
    }

    /// The tools of a running server that `filter` allows. Empty while the
    /// server isn't running.
    pub fn tools(&self, server: &str, filter: &dyn ToolFilter) -> Result<Vec<Tool>> {
        let shared = self.shared(server)?;
        let tools = shared.tools.locked().clone();
        Ok(tools.iter().filter(|tool| filter.allows(server, &tool.name)).cloned().collect())
    }

    /// Calls a tool. Refused without contacting the server when `filter`
    /// doesn't allow it.
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<JsonObject>,
        filter: &dyn ToolFilter,
    ) -> Result<CallToolResult> {
        if !filter.allows(server, tool) {
            return Err(Error::NotAllowed { server: server.to_owned(), tool: tool.to_owned() });
        }
        let peer = self.shared(server)?.peer()?;
        let mut params = CallToolRequestParams::new(tool.to_owned());
        params.arguments = arguments;
        peer.call_tool(params).await.map_err(|source| Error::Service { server: server.to_owned(), source: Box::new(source) })
    }

    /// The resources a running server declares. Empty when it has no
    /// resources capability.
    pub async fn list_resources(&self, server: &str) -> Result<Vec<Resource>> {
        let peer = self.shared(server)?.peer()?;
        let declares = peer.peer_info().is_none_or(|info| info.capabilities.resources.is_some());
        if !declares {
            return Ok(Vec::new());
        }
        peer.list_all_resources().await.map_err(|source| Error::Service { server: server.to_owned(), source: Box::new(source) })
    }

    pub async fn read_resource(&self, server: &str, uri: &str) -> Result<Vec<ResourceContents>> {
        let peer = self.shared(server)?.peer()?;
        let result = peer
            .read_resource(ReadResourceRequestParams::new(uri))
            .await
            .map_err(|source| Error::Service { server: server.to_owned(), source: Box::new(source) })?;
        Ok(result.contents)
    }

    /// The `surface://` resources a running server declares.
    pub async fn surfaces(&self, server: &str) -> Result<Vec<(SurfaceUri, Resource)>> {
        let resources = self.list_resources(server).await?;
        Ok(resources.into_iter().filter_map(|resource| Some((surface_uri(&resource)?, resource))).collect())
    }

    /// Connects to `spec` once, lists its tools and disconnects: the check
    /// run before a server is saved.
    pub async fn probe(&self, spec: &ServerSpec) -> Result<Vec<Tool>> {
        spec.validate()?;
        let Connection { mut service, .. } =
            connect::connect(spec, (), &self.inner.http, self.inner.options.connect_timeout).await?;
        let tools = service.list_all_tools().await;
        let _ = service.close().await;
        tools.map_err(|source| Error::Service { server: spec.name.clone(), source: Box::new(source) })
    }

    fn shared(&self, server: &str) -> Result<Arc<Shared>> {
        self.inner.servers.locked()
            .get(server)
            .map(|entry| entry.shared.clone())
            .ok_or_else(|| Error::UnknownServer(server.to_owned()))
    }

    fn spawn(&self, entry: &Entry) -> Run {
        let stop = CancellationToken::new();
        let supervisor = Supervisor {
            spec: entry.spec.clone(),
            shared: entry.shared.clone(),
            http: self.inner.http.clone(),
            options: self.inner.options.clone(),
            stop: stop.clone(),
        };
        Run { stop, task: tokio::spawn(supervisor.run()) }
    }
}

async fn finish(run: Run) {
    run.stop.cancel();
    let _ = run.task.await;
}

impl Drop for Inner {
    /// Without [`Manager::shutdown`], supervisors still close their children,
    /// just without anyone waiting for it.
    fn drop(&mut self) {
        for entry in self.servers.locked().values() {
            if let Some(run) = &entry.run {
                run.stop.cancel();
            }
        }
    }
}

/// Keeps one server connected until stopped.
struct Supervisor {
    spec: ServerSpec,
    shared: Arc<Shared>,
    http: reqwest::Client,
    options: Options,
    stop: CancellationToken,
}

impl Supervisor {
    async fn run(self) {
        let mut attempt = 0;
        loop {
            self.shared.set_status(Status::Starting);
            let handler = Client { shared: self.shared.clone() };
            let connecting = connect::connect(&self.spec, handler, &self.http, self.options.connect_timeout);
            let connected = tokio::select! {
                () = self.stop.cancelled() => break,
                connected = connecting => connected,
            };
            match connected {
                Ok(connection) => {
                    let started = Instant::now();
                    match self.serve(connection).await {
                        Ended::Stopped => break,
                        Ended::Failed(message) => self.shared.set_status(Status::Error(message)),
                    }
                    if started.elapsed() >= self.options.backoff.max {
                        attempt = 0;
                    }
                }
                Err(e) => self.shared.set_status(Status::Error(e.to_string())),
            }
            let delay = self.options.backoff.delay(attempt);
            attempt = attempt.saturating_add(1);
            tracing::info!(server = self.spec.name, "restarting in {delay:?}");
            tokio::select! {
                () = self.stop.cancelled() => break,
                () = tokio::time::sleep(delay) => {}
            }
        }
        self.shared.set_status(Status::Stopped);
    }

    /// Runs one connection until it ends.
    async fn serve(&self, connection: Connection<Client>) -> Ended {
        let Connection { mut service, stderr } = connection;
        let listed = tokio::select! {
            () = self.stop.cancelled() => {
                let _ = service.close().await;
                return Ended::Stopped;
            }
            listed = service.list_all_tools() => listed,
        };
        let tools = match listed {
            Ok(tools) => tools,
            Err(e) => {
                let _ = service.close().await;
                return Ended::Failed(format!("listing tools: {e}"));
            }
        };
        self.shared.set_tools(tools);
        *self.shared.peer.locked() = Some(service.peer().clone());
        self.shared.set_status(Status::Running);

        let cancel = service.cancellation_token();
        let waiting = service.waiting();
        tokio::pin!(waiting);
        let stopped = tokio::select! {
            _ = &mut waiting => false,
            () = self.stop.cancelled() => {
                cancel.cancel();
                let _ = waiting.await;
                true
            }
        };
        *self.shared.peer.locked() = None;
        self.shared.set_tools(Vec::new());
        if stopped {
            return Ended::Stopped;
        }
        let last = match stderr {
            Some(stderr) => stderr.last_line().await,
            None => None,
        };
        Ended::Failed(match last {
            Some(line) => format!("the server exited: {line}"),
            None => "the server closed the connection".to_owned(),
        })
    }
}

enum Ended {
    Stopped,
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let backoff = Backoff { initial: Duration::from_millis(100), max: Duration::from_secs(1) };
        let delays: Vec<_> = (0..6).map(|attempt| backoff.delay(attempt).as_millis()).collect();
        assert_eq!(delays, [100, 200, 400, 800, 1000, 1000]);
        assert_eq!(backoff.delay(u32::MAX), Duration::from_secs(1));
    }
}
