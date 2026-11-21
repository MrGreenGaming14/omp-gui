//! The client against fake MCP servers: this test binary re-run as a stdio
//! child (when `LOA_MCP_FAKE_SERVER` is set), and the same server handler
//! behind an in-process streamable HTTP endpoint on a loopback port.
//!
//! The harness is libtest-mimic so `main` can turn into the fake server
//! before libtest prints anything on stdout.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use libtest_mimic::{Arguments, Trial};
use loa_mcp::import::{ConfigFile, Outcome};
use loa_mcp::model::ResourceContents;
use loa_mcp::{
    AllowAll, Backoff, Error, Event, HttpSpec, Manager, MemoryStore, Options, Registry, ServerSpec, Status, StdioSpec,
    Transport,
};

const ROLE_ENV: &str = "LOA_MCP_FAKE_SERVER";
/// Upper bound for anything a test waits on; never a synchronization sleep.
const LIMIT: Duration = Duration::from_secs(20);
/// What the fake's `vault_code` tool returns; a model can't guess it.
const VAULT_CODE: &str = "vault code 7731-QZ";
/// A 1x1 PNG the fake's `picture` tool returns.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49,
    0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d,
    0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn main() {
    if std::env::var_os(ROLE_ENV).is_some() {
        fake::serve_stdio();
        return;
    }
    let args = Arguments::from_args();
    let tests = vec![
        trial("stdio_server_lists_calls_and_reads_resources", stdio_server_lists_calls_and_reads_resources),
        trial("filter_hides_and_refuses_tools", filter_hides_and_refuses_tools),
        trial("crashed_server_restarts_with_its_error_shown", crashed_server_restarts_with_its_error_shown),
        trial("tool_list_change_is_picked_up", tool_list_change_is_picked_up),
        trial("missing_command_reports_error_and_stops_cleanly", missing_command_reports_error_and_stops_cleanly),
        trial("http_server_is_validated_with_its_headers", http_server_is_validated_with_its_headers),
        trial("registry_edits_and_removes_servers", registry_edits_and_removes_servers),
        trial("registry_import_starts_new_servers_once", registry_import_starts_new_servers_once),
        trial("host_tools_route_calls_through_the_filter", host_tools_route_calls_through_the_filter),
    ];
    libtest_mimic::run(&args, tests).exit();
}

fn trial<F: Future<Output = ()> + 'static>(name: &str, test: fn() -> F) -> Trial {
    Trial::test(name, move || {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|e| e.to_string())?;
        runtime.block_on(test());
        Ok(())
    })
}

fn options() -> Options {
    Options {
        backoff: Backoff { initial: Duration::from_millis(10), max: Duration::from_millis(200) },
        connect_timeout: LIMIT,
    }
}

fn fake_stdio() -> StdioSpec {
    StdioSpec {
        command: std::env::current_exe().unwrap().to_string_lossy().into_owned(),
        env: BTreeMap::from([(ROLE_ENV.to_owned(), "1".to_owned())]),
        ..StdioSpec::default()
    }
}

fn fake_spec(name: &str) -> ServerSpec {
    ServerSpec::new(name, Transport::Stdio(fake_stdio())).unwrap()
}

async fn wait_status(manager: &Manager, name: &str, wanted: impl Fn(&Status) -> bool) -> Status {
    let mut status = manager.watch(name).unwrap();
    let seen = tokio::time::timeout(LIMIT, status.wait_for(|status| wanted(status))).await;
    seen.expect("status didn't arrive in time").unwrap().clone()
}

async fn next_event(events: &mut tokio::sync::broadcast::Receiver<Event>, wanted: impl Fn(&Event) -> bool) -> Event {
    tokio::time::timeout(LIMIT, async {
        loop {
            let event = events.recv().await.unwrap();
            if wanted(&event) {
                return event;
            }
        }
    })
    .await
    .expect("event didn't arrive in time")
}

fn text(result: &loa_mcp::model::CallToolResult) -> &str {
    result.content[0].as_text().map(|text| text.text.as_str()).unwrap()
}

async fn call_text(manager: &Manager, server: &str, tool: &str, arguments: serde_json::Value) -> String {
    let arguments = arguments.as_object().cloned();
    let result = manager.call_tool(server, tool, arguments, &AllowAll).await.unwrap();
    assert_ne!(result.is_error, Some(true));
    text(&result).to_owned()
}

fn running(status: &Status) -> bool {
    *status == Status::Running
}

async fn stdio_server_lists_calls_and_reads_resources() {
    let manager = Manager::new(options()).unwrap();
    manager.add(fake_spec("fake")).unwrap();
    wait_status(&manager, "fake", running).await;

    let names: Vec<_> = manager.tools("fake", &AllowAll).unwrap().into_iter().map(|tool| tool.name).collect();
    assert_eq!(names, ["echo", "pid", "crash", "grow", "vault_code", "picture"]);

    assert_eq!(call_text(&manager, "fake", "echo", serde_json::json!({"text": "hello"})).await, "hello");

    let resources = manager.list_resources("fake").await.unwrap();
    assert_eq!(resources.len(), 2);
    let surfaces = manager.surfaces("fake").await.unwrap();
    assert_eq!(surfaces.len(), 1);
    assert_eq!(surfaces[0].0.as_str(), "surface://fake/tab1");
    assert_eq!(surfaces[0].1.name, "Tab 1");

    let contents = manager.read_resource("fake", "file:///notes.txt").await.unwrap();
    let [ResourceContents::TextResourceContents { text, .. }] = contents.as_slice() else {
        panic!("expected one text resource, got {contents:?}");
    };
    assert_eq!(text, "the notes");

    let pid: u32 = call_text(&manager, "fake", "pid", serde_json::json!({})).await.parse().unwrap();
    manager.shutdown().await;
    assert_eq!(manager.status("fake"), Some(Status::Stopped));
    assert!(!PathBuf::from(format!("/proc/{pid}")).exists(), "the child {pid} outlived shutdown");
    assert!(matches!(
        manager.call_tool("fake", "echo", None, &AllowAll).await,
        Err(Error::NotRunning(_))
    ));
}

async fn filter_hides_and_refuses_tools() {
    let manager = Manager::new(options()).unwrap();
    manager.add(fake_spec("fake")).unwrap();
    wait_status(&manager, "fake", running).await;

    // A peer whose grant allows only `echo` on `fake`.
    let grant = |server: &str, tool: &str| server == "fake" && tool == "echo";
    let names: Vec<_> = manager.tools("fake", &grant).unwrap().into_iter().map(|tool| tool.name).collect();
    assert_eq!(names, ["echo"]);

    let view = manager.view(&grant);
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].spec.name, "fake");
    let allowed: Vec<_> = view[0].tools.iter().map(|tool| (tool.tool.name.as_ref(), tool.allowed)).collect();
    assert_eq!(
        allowed,
        [("echo", true), ("pid", false), ("crash", false), ("grow", false), ("vault_code", false), ("picture", false)]
    );

    let refused = manager.call_tool("fake", "crash", None, &grant).await;
    assert!(matches!(refused, Err(Error::NotAllowed { .. })));
    // The refused crash never reached the server.
    assert_eq!(manager.status("fake"), Some(Status::Running));
    manager.shutdown().await;
}

async fn crashed_server_restarts_with_its_error_shown() {
    let manager = Manager::new(options()).unwrap();
    manager.add(fake_spec("fake")).unwrap();
    wait_status(&manager, "fake", running).await;
    let first: u32 = call_text(&manager, "fake", "pid", serde_json::json!({})).await.parse().unwrap();

    let mut events = manager.events();
    assert!(manager.call_tool("fake", "crash", None, &AllowAll).await.is_err());
    let failed = next_event(&mut events, |event| matches!(event, Event::Status { status: Status::Error(_), .. })).await;
    let Event::Status { status: Status::Error(message), .. } = failed else { unreachable!() };
    assert!(message.contains("crash requested"), "{message}");
    next_event(&mut events, |event| matches!(event, Event::Status { status: Status::Running, .. })).await;

    let second: u32 = call_text(&manager, "fake", "pid", serde_json::json!({})).await.parse().unwrap();
    assert_ne!(first, second);
    assert_eq!(call_text(&manager, "fake", "echo", serde_json::json!({"text": "back"})).await, "back");
    manager.shutdown().await;
}

async fn tool_list_change_is_picked_up() {
    let manager = Manager::new(options()).unwrap();
    manager.add(fake_spec("fake")).unwrap();
    wait_status(&manager, "fake", running).await;

    let mut events = manager.events();
    call_text(&manager, "fake", "grow", serde_json::json!({})).await;
    next_event(&mut events, |event| matches!(event, Event::ToolsChanged { server } if server == "fake")).await;
    let names: Vec<_> = manager.tools("fake", &AllowAll).unwrap().into_iter().map(|tool| tool.name).collect();
    assert!(names.iter().any(|name| name == "grown"), "{names:?}");
    manager.shutdown().await;
}

async fn missing_command_reports_error_and_stops_cleanly() {
    let manager = Manager::new(options()).unwrap();
    let spec = ServerSpec::new("ghost", Transport::Stdio(StdioSpec::parse("/nonexistent/loa-mcp-ghost --x").unwrap())).unwrap();
    manager.add(spec).unwrap();
    let status = wait_status(&manager, "ghost", |status| matches!(status, Status::Error(_))).await;
    let Status::Error(message) = status else { unreachable!() };
    assert!(message.contains("/nonexistent/loa-mcp-ghost"), "{message}");

    manager.stop("ghost").await.unwrap();
    assert_eq!(manager.status("ghost"), Some(Status::Stopped));
}

async fn http_server_is_validated_with_its_headers() {
    let url = fake::serve_http("secret").await;
    let registry = Registry::open(MemoryStore::default(), Manager::new(options()).unwrap()).await.unwrap();

    let without = ServerSpec::new("web", Transport::Http(HttpSpec::new(url.clone(), []).unwrap())).unwrap();
    assert!(matches!(registry.add(without).await, Err(Error::Connect { .. })));
    assert!(registry.specs().await.is_empty());
    assert!(registry.manager().status("web").is_none());

    let headers = [("X-Api-Key".to_owned(), "secret".to_owned())];
    let with = ServerSpec::new("web", Transport::Http(HttpSpec::new(url, headers).unwrap())).unwrap();
    let tools = registry.add(with.clone()).await.unwrap();
    assert_eq!(tools.len(), 6);
    assert_eq!(registry.specs().await, [with]);

    let manager = registry.manager();
    wait_status(manager, "web", running).await;
    assert_eq!(call_text(manager, "web", "echo", serde_json::json!({"text": "over http"})).await, "over http");
    let contents = manager.read_resource("web", "file:///notes.txt").await.unwrap();
    assert_eq!(contents.len(), 1);
    manager.shutdown().await;
}

async fn registry_edits_and_removes_servers() {
    let store = Arc::new(MemoryStore::default());
    let registry = Registry::open(store.clone(), Manager::new(options()).unwrap()).await.unwrap();
    registry.add(fake_spec("one")).await.unwrap();
    assert!(matches!(registry.add(fake_spec("one")).await, Err(Error::NameTaken(_))));

    let broken = ServerSpec::new("two", Transport::Stdio(StdioSpec::parse("/nonexistent/loa-mcp-ghost").unwrap())).unwrap();
    assert!(matches!(registry.edit("one", broken).await, Err(Error::Connect { .. })));
    assert_eq!(store.servers(), [fake_spec("one")]);

    let mut renamed = fake_spec("two");
    let Transport::Stdio(stdio) = &mut renamed.transport else { unreachable!() };
    stdio.env.insert("EXTRA".to_owned(), "1".to_owned());
    registry.edit("one", renamed.clone()).await.unwrap();
    assert_eq!(store.servers(), [renamed]);
    assert!(registry.manager().status("one").is_none());
    wait_status(registry.manager(), "two", running).await;

    registry.set_enabled("two", false).await.unwrap();
    assert_eq!(registry.manager().status("two"), Some(Status::Stopped));
    assert!(!store.servers()[0].enabled);

    registry.remove("two").await.unwrap();
    assert!(store.servers().is_empty());
    assert!(registry.manager().specs().is_empty());
}

async fn registry_import_starts_new_servers_once() {
    let dir = tempfile::tempdir().unwrap();
    let stdio = fake_stdio();
    let config = serde_json::json!({"mcpServers": {"imported": {"command": stdio.command, "env": stdio.env}}});
    let path = dir.path().join(".mcp.json");
    std::fs::write(&path, config.to_string()).unwrap();
    let files = [ConfigFile::ClaudeProject(path)];

    let store = Arc::new(MemoryStore::default());
    let registry = Registry::open(store.clone(), Manager::new(options()).unwrap()).await.unwrap();
    let report = registry.import(&files).await.unwrap();
    assert!(matches!(report.entries[0].outcome, Outcome::Imported(_)));
    assert_eq!(store.servers().len(), 1);
    wait_status(registry.manager(), "imported", running).await;

    let again = registry.import(&files).await.unwrap();
    assert_eq!(again.entries[0].outcome, Outcome::Duplicate { existing: "imported".to_owned() });
    assert_eq!(store.servers().len(), 1);
    registry.manager().shutdown().await;
}

async fn host_tools_route_calls_through_the_filter() {
    use loa_adapter::{HostToolContent, HostTools};
    use loa_mcp::host_tools::McpHostTools;

    let manager = Manager::new(options()).unwrap();
    manager.add(fake_spec("fake")).unwrap();
    manager.add(ServerSpec { enabled: false, ..fake_spec("off") }).unwrap();
    wait_status(&manager, "fake", running).await;
    let filter = |server: &str, tool: &str| server == "fake" && tool != "crash";
    let tools = McpHostTools::new(manager.clone(), Arc::new(filter));
    let names: Vec<_> = tools.specs().iter().map(|spec| spec.name.as_str()).collect();
    assert_eq!(names, ["fake__echo", "fake__pid", "fake__grow", "fake__vault_code", "fake__picture"]);
    let vault = &tools.specs()[3];
    assert_eq!(vault.description, "[fake] Returns the current vault code. Takes no arguments.");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&vault.input_schema).unwrap()["type"], "object");

    assert_eq!(tools.call("s", "fake__echo", r#"{"text":"hi"}"#).await.unwrap(), "hi");
    assert_eq!(tools.call("s", "fake__vault_code", "{}").await.unwrap(), VAULT_CODE);
    let blocks = tools.call_content("s", "fake__picture", "{}").await.unwrap();
    assert_eq!(blocks[0], HostToolContent::Text("a picture".into()));
    assert!(matches!(&blocks[1], HostToolContent::Image { mime, data } if mime == "image/png" && data.as_ref() == PNG));
    assert!(tools.call("s", "fake__crash", "{}").await.unwrap_err().contains("unknown MCP tool"), "filtered out");
    assert!(tools.call("s", "fake__echo", "[1]").await.is_err());
    manager.shutdown().await;
}

/// The fake MCP server.
mod fake {
    use super::*;
    use axum::response::IntoResponse;
    use rmcp::handler::server::ServerHandler;
    use rmcp::model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData, JsonObject,
        ListResourcesResult, ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
        ReadResourceResult, Resource, ServerCapabilities, ServerConfig, Tool,
    };
    use rmcp::service::RequestContext;
    use rmcp::transport::StreamableHttpService;
    use rmcp::transport::streamable_http_server::StreamableHttpServerConfig;
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::{RoleServer, ServiceExt};

    #[derive(Clone, Default)]
    pub struct Fake {
        grown: Arc<AtomicBool>,
    }

    fn tool(name: &'static str) -> Tool {
        let schema: JsonObject = serde_json::from_value(serde_json::json!({"type": "object"})).unwrap();
        Tool::new(name, name, schema)
    }

    impl ServerHandler for Fake {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(
                ServerCapabilities::builder().enable_tools().enable_tool_list_changed().enable_resources().build(),
            )
        }

        async fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            let mut tools = vec![tool("echo"), tool("pid"), tool("crash"), tool("grow")];
            tools.push(Tool::new(
                "vault_code",
                "Returns the current vault code. Takes no arguments.",
                serde_json::from_value::<JsonObject>(serde_json::json!({"type": "object", "properties": {}})).unwrap(),
            ));
            tools.push(tool("picture"));
            if self.grown.load(Ordering::SeqCst) {
                tools.push(tool("grown"));
            }
            Ok(ListToolsResult::with_all_items(tools))
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            let reply = |text: String| Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into());
            match request.name.as_ref() {
                "echo" => {
                    let arguments = request.arguments.unwrap_or_default();
                    reply(arguments.get("text").and_then(|text| text.as_str()).unwrap_or_default().to_owned())
                }
                "pid" => reply(std::process::id().to_string()),
                "crash" => {
                    eprintln!("fatal: crash requested");
                    std::process::exit(3);
                }
                "grow" => {
                    self.grown.store(true, Ordering::SeqCst);
                    context.peer.notify_tool_list_changed().await.map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    reply("grown".to_owned())
                }
                "vault_code" => reply(super::VAULT_CODE.to_owned()),
                "picture" => {
                    use base64::Engine;
                    let png = base64::engine::general_purpose::STANDARD.encode(super::PNG);
                    Ok(CallToolResult::success(vec![ContentBlock::text("a picture"), ContentBlock::image(png, "image/png")]).into())
                }
                other => Err(ErrorData::invalid_params(format!("no tool {other}"), None)),
            }
        }

        async fn list_resources(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListResourcesResult, ErrorData> {
            Ok(ListResourcesResult::with_all_items(vec![
                Resource::new("surface://fake/tab1", "Tab 1").with_mime_type("image/png"),
                Resource::new("file:///notes.txt", "notes"),
            ]))
        }

        async fn read_resource(
            &self,
            request: ReadResourceRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<ReadResourceResponse, ErrorData> {
            match request.uri.as_str() {
                "file:///notes.txt" => Ok(ReadResourceResult::new(vec![ResourceContents::text("the notes", request.uri)]).into()),
                _ => Err(ErrorData::resource_not_found(request.uri, None)),
            }
        }
    }

    pub fn serve_stdio() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let service = Fake::default().serve(rmcp::transport::stdio()).await.unwrap();
            let _ = service.waiting().await;
        });
    }

    /// Serves the fake on `127.0.0.1:<free port>/mcp`, refusing requests
    /// without `X-Api-Key: <key>`. Returns the url.
    pub async fn serve_http(key: &'static str) -> String {
        let service: StreamableHttpService<Fake, LocalSessionManager> = StreamableHttpService::new(
            || Ok(Fake::default()),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default(),
        );
        let check = move |request: axum::extract::Request, next: axum::middleware::Next| async move {
            if request.headers().get("x-api-key").is_some_and(|value| value == key) {
                next.run(request).await
            } else {
                axum::http::StatusCode::UNAUTHORIZED.into_response()
            }
        };
        let app = axum::Router::new().nest_service("/mcp", service).layer(axum::middleware::from_fn(check));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        format!("http://{address}/mcp")
    }
}
