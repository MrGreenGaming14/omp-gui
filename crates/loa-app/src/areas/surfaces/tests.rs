//! The Surfaces tab driven through `App::update`: documents, VMs, browser
//! tabs and MCP declared surfaces open in the tab, are recorded in the
//! session, and the agent's `surface_*` tools reach them.

use std::sync::Arc;
use std::time::Duration;

use iced::futures::StreamExt;
use iced::{Point, Size, Task};
use iced_runtime::Action;
use loa_adapter::{Capabilities, HostToolContent, SessionConfig};
use loa_core::{Dom, ElementKind, attr};
use loa_mcp::{HttpSpec, ServerSpec, Transport};
use loa_surface::{MouseButton, SurfaceMode, SurfaceUri};
use loa_surface_hub::{HubConfig, McpEndpoint, Origin, SurfaceHub, SurfaceTools};
use loa_ui_palette as palette;
use loa_ui_surface::{PointerInput, panel, picker};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use super::{HubWatch, McpWatch, State, TAB, hub_events, mcp_events};
use crate::app::tests::fake_app;
use crate::app::{App, Message as AppMessage};
use crate::area::{Area, Target};

const PATIENCE: Duration = Duration::from_secs(30);

/// Runs an app the way the iced runtime does: every task's messages go back
/// through `App::update`, and the area's subscriptions stream in.
struct Driver {
    app: App,
    out: mpsc::UnboundedSender<AppMessage>,
    messages: mpsc::UnboundedReceiver<AppMessage>,
}

impl Driver {
    /// A fake-harness app whose Surfaces area runs on `hub`.
    fn new(dir: &std::path::Path, hub: Arc<SurfaceHub>) -> Self {
        Self::over(fake_app(dir, &["fake-a"]), hub)
    }

    /// `app` with its Surfaces area on `hub`.
    fn over(mut app: App, hub: Arc<SurfaceHub>) -> Self {
        let (state, task) = State::with_hub(Arc::clone(&hub), &mut app.ctx);
        app.areas.surfaces = state;
        let (out, messages) = mpsc::unbounded_channel();
        let driver = Self { app, out, messages };
        driver.spawn(task.map(AppMessage::Surfaces));
        driver.forward(hub_events(&HubWatch(hub)));
        driver.forward(mcp_events(&McpWatch(driver.app.ctx.services.mcp.clone())));
        driver
    }

    fn forward(&self, stream: impl iced::futures::Stream<Item = super::Message> + Send + 'static) {
        let out = self.out.clone();
        tokio::spawn(async move {
            let mut stream = std::pin::pin!(stream);
            while let Some(message) = stream.next().await {
                if out.send(AppMessage::Surfaces(message)).is_err() {
                    return;
                }
            }
        });
    }

    fn spawn(&self, task: Task<AppMessage>) {
        let Some(mut stream) = iced_runtime::task::into_stream(task) else { return };
        let out = self.out.clone();
        tokio::spawn(async move {
            while let Some(action) = stream.next().await {
                if let Action::Output(message) = action
                    && out.send(message).is_err()
                {
                    return;
                }
            }
        });
    }

    fn update(&mut self, message: AppMessage) {
        let task = self.app.update(message);
        self.spawn(task);
    }

    fn surfaces(&mut self, message: panel::Message) {
        self.update(AppMessage::Surfaces(super::Message::Panel(message)));
    }

    /// Delivers messages until `done` holds.
    async fn until(&mut self, what: &str, mut done: impl FnMut(&App) -> bool) {
        let wait = async {
            while !done(&self.app) {
                let message = self.messages.recv().await.expect("the app runs");
                let task = self.app.update(message);
                self.spawn(task);
            }
        };
        tokio::time::timeout(PATIENCE, wait).await.unwrap_or_else(|_| panic!("timed out waiting until {what}"));
    }

    fn area(&self) -> &State {
        &self.app.areas.surfaces
    }
}

/// The `<surface>` elements of the active session, from its writer: uri,
/// status and parent.
fn recorded(app: &App) -> Vec<(String, String, loa_core::NodeId)> {
    let session = app.ctx.sessions.active().expect("a session");
    session.writer().with_dom(|dom: &Dom| {
        dom.iter()
            .filter(|node| node.kind == ElementKind::Surface)
            .map(|node| {
                let uri = node.str_attr(attr::URI).unwrap_or_default().to_owned();
                let status = node.str_attr(attr::STATUS).unwrap_or_default().to_owned();
                (uri, status, node.parent)
            })
            .collect()
    })
}

fn png(path: &std::path::Path, color: [u8; 4]) {
    image::RgbaImage::from_pixel(24, 12, image::Rgba(color)).save(path).unwrap();
}

fn hub(dir: &std::path::Path, browser: bool) -> Arc<SurfaceHub> {
    let mut config = HubConfig::discover(&dir.join("hub"));
    match &mut config.browser {
        Some(b) if browser => {
            b.width = 480;
            b.height = 320;
        }
        _ => config.browser = None,
    }
    SurfaceHub::new(config)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_document_picked_in_the_tab_renders_and_is_recorded_in_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("chart.png");
    png(&image, [0, 0, 255, 255]);
    let mut driver = Driver::new(dir.path(), hub(dir.path(), false));

    // The palette command shows the tab on its launcher.
    driver.update(AppMessage::Palette(palette::Message::Open));
    driver.update(AppMessage::Palette(palette::Message::QueryChanged("Open document surface".into())));
    driver.update(AppMessage::Palette(palette::Message::Submit));
    assert_eq!(driver.app.ctx.shell.active_tab(), Some(TAB));
    assert!(driver.area().panel().active().is_none(), "the launcher shows");

    // Opened before any session: recorded in the next one.
    driver.surfaces(panel::Message::Picker(picker::Message::Choose(image.clone())));
    driver.until("the document is shown", |app| app.areas.surfaces.panel().active().is_some()).await;
    let uri = driver.area().panel().active().unwrap().clone();
    assert_eq!(uri.server(), "docs");
    driver
        .until("the page is rendered", |app| {
            app.areas.surfaces.panel().surface(&uri).is_some_and(|s| s.info().mode == SurfaceMode::Document && s.page_count() == Some(1))
        })
        .await;

    driver.update(AppMessage::NewSession);
    driver.until("the session runs", |app| app.ctx.sessions.active().is_some()).await;
    let rows = recorded(&driver.app);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].0.as_str(), rows[0].1.as_str()), (uri.as_str(), "open"));

    // The tab's close button closes it in the hub and in the session.
    driver.surfaces(panel::Message::Close(uri.clone()));
    driver.until("the surface is closed", |app| app.areas.surfaces.hub().opened(&uri).is_none()).await;
    driver.until("the session says closed", |app| recorded(app)[0].1 == "closed").await;
    assert_eq!(driver.area().panel().open_surfaces().count(), 0);
}

/// A one-connection RFB 3.8 server: no security, `width` x `height`, one
/// solid raw update. Collects what the client sends after the setup.
async fn fake_rfb(width: u16, height: u16, color: [u8; 3]) -> (String, Arc<std::sync::Mutex<Vec<u8>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let received = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = received.clone();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 12];
        stream.write_all(b"RFB 003.008\n").await.unwrap();
        stream.read_exact(&mut buf).await.unwrap();
        stream.write_all(&[1, 1]).await.unwrap();
        stream.read_exact(&mut buf[..1]).await.unwrap();
        stream.write_all(&0u32.to_be_bytes()).await.unwrap();
        stream.read_exact(&mut buf[..1]).await.unwrap();
        let mut init = Vec::new();
        init.extend_from_slice(&width.to_be_bytes());
        init.extend_from_slice(&height.to_be_bytes());
        init.extend_from_slice(&[32, 24, 1, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
        init.extend_from_slice(&4u32.to_be_bytes());
        init.extend_from_slice(b"desk");
        stream.write_all(&init).await.unwrap();
        // SetPixelFormat (20), SetEncodings (4 + 7 * 4), FramebufferUpdateRequest (10).
        let mut setup = vec![0u8; 62];
        stream.read_exact(&mut setup).await.unwrap();
        let mut update = vec![0, 0, 0, 1, 0, 0, 0, 0];
        update.extend_from_slice(&width.to_be_bytes());
        update.extend_from_slice(&height.to_be_bytes());
        update.extend_from_slice(&0i32.to_be_bytes());
        for _ in 0..usize::from(width) * usize::from(height) {
            update.extend_from_slice(&[color[0], color[1], color[2], 0]);
        }
        stream.write_all(&update).await.unwrap();
        let mut chunk = [0u8; 256];
        while let Ok(n) = stream.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    });
    (address, received)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_vm_typed_as_host_and_port_streams_into_the_tab_and_takes_clicks() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::new(dir.path(), hub(dir.path(), false));
    driver.update(AppMessage::NewSession);
    driver.until("the session runs", |app| app.ctx.sessions.active().is_some()).await;

    let (address, received) = fake_rfb(8, 4, [0, 200, 0]).await;
    driver.surfaces(panel::Message::Address(address.clone()));
    driver.surfaces(panel::Message::Connect);
    let uri = SurfaceUri::new("vm", &address).unwrap();
    driver
        .until("the desktop's first frame is drawn", |app| {
            app.areas.surfaces.panel().surface(&uri).and_then(|s| s.framebuffer()).is_some_and(|fb| fb.width() == 8)
        })
        .await;
    let session_record = recorded(&driver.app);
    assert!(session_record.iter().any(|(u, status, _)| u == uri.as_str() && status == "open"), "{session_record:?}");

    // A click on the panel reaches the RFB server as a pointer event.
    let size = Size::new(8.0, 4.0);
    driver.surfaces(panel::Message::Surface(uri.clone(), loa_ui_surface::Message::Panel(size)));
    let press = PointerInput::Button { at: Point::new(3.0, 2.0), button: MouseButton::Left, pressed: true };
    driver.surfaces(panel::Message::Surface(uri.clone(), loa_ui_surface::Message::Pointer { panel: size, input: press }));
    let pressed_at = [5u8, 1, 0, 3, 0, 2];
    tokio::time::timeout(PATIENCE, async {
        while !received.lock().unwrap().windows(6).any(|w| w == pressed_at) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the click reached the VM");

    // Closed, the endpoint stays listed on the launcher for reconnecting.
    driver.surfaces(panel::Message::Close(uri.clone()));
    driver
        .until("the endpoint is listed", |app| app.areas.surfaces.panel().available().iter().any(|a| a.info.uri == uri))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_tab_opens_from_a_url_takes_clicks_and_the_agent_drives_it() {
    let dir = tempfile::tempdir().unwrap();
    let hub = hub(dir.path(), true);
    if hub.browser_executable().is_none() {
        eprintln!("SKIPPED: no Chrome or Chromium installed");
        return;
    }
    let mut driver = Driver::new(dir.path(), Arc::clone(&hub));
    driver.update(AppMessage::NewSession);
    driver.until("the session runs", |app| app.ctx.sessions.active().is_some()).await;

    let page = "data:text/html,<title>Red</title><body style='margin:0;height:100vh;background:rgb(255,0,0)' onmousedown=\"document.body.style.background='rgb(0,0,255)'\"></body>";
    driver.surfaces(panel::Message::Url(page.into()));
    driver.surfaces(panel::Message::OpenUrl);
    driver.until("the tab is shown", |app| app.areas.surfaces.panel().active().is_some()).await;
    let uri = driver.area().panel().active().unwrap().clone();
    assert_eq!(uri.server(), "browser");

    let size = Size::new(480.0, 320.0);
    driver.surfaces(panel::Message::Surface(uri.clone(), loa_ui_surface::Message::Panel(size)));
    driver.until("a frame is shown", |app| app.areas.surfaces.panel().surface(&uri).and_then(|s| s.shown_seq()).is_some()).await;
    let press = PointerInput::Button { at: Point::new(240.0, 160.0), button: MouseButton::Left, pressed: true };
    driver.surfaces(panel::Message::Surface(uri.clone(), loa_ui_surface::Message::Pointer { panel: size, input: press }));
    let blue = |app: &App| {
        let Some(fb) = app.areas.surfaces.panel().surface(&uri).and_then(|s| s.framebuffer()) else { return false };
        let i = ((fb.height() / 2) as usize * fb.width() as usize + (fb.width() / 2) as usize) * 4;
        let px = &fb.pixels()[i..i + 3];
        px[2] > 200 && px[0] < 60
    };
    driver
        .until("the click turned the page blue and was measured", |app| {
            blue(app) && app.areas.surfaces.panel().surface(&uri).and_then(|s| s.latency()).is_some()
        })
        .await;

    // The session's host tools: the agent screenshots the tab and opens another one.
    let capabilities = Capabilities { host_tools: true, ..Capabilities::default() };
    let target = Target { harness: "fake-a", capabilities: &capabilities, resume: false };
    let set = driver.area().host_tools(&SessionConfig::default(), &target, &driver.app.ctx).expect("host tools");
    assert!(set.specs.iter().any(|spec| spec.name == "surface_screenshot"));
    let session = driver.app.ctx.sessions.active_id().unwrap().to_string();
    let args = json!({ "uri": uri.as_str() }).to_string();
    let shot = set.handler.call_content(&session, "surface_screenshot", &args).await.unwrap();
    assert!(shot.iter().any(|c| matches!(c, HostToolContent::Image { data, .. } if data.starts_with(b"\x89PNG"))));
    let args = json!({ "url": "data:text/html,<title>Agent</title><h1>agent</h1>" }).to_string();
    set.handler.call_content(&session, "surface_open_browser", &args).await.unwrap();
    driver
        .until("the agent's tab shows in the panel", |app| app.areas.surfaces.panel().open_surfaces().count() == 2)
        .await;
    let rows = recorded(&driver.app);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows.iter().all(|(_, status, _)| status == "open"));
    hub.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn surfaces_an_mcp_server_declares_are_listed_and_open() {
    let dir = tempfile::tempdir().unwrap();
    // The "third party" server: another hub's MCP endpoint declaring a document.
    let remote = hub(&dir.path().join("remote"), false);
    let image = dir.path().join("remote.png");
    png(&image, [0, 160, 0, 255]);
    let doc = remote.open_document(&image).await.unwrap();
    let endpoint = McpEndpoint::start(SurfaceTools::new(Arc::clone(&remote))).await.unwrap();

    let mut driver = Driver::new(dir.path(), hub(dir.path(), false));
    let auth = vec![("Authorization".to_owned(), format!("Bearer {}", endpoint.token()))];
    let spec = ServerSpec::new("deck", Transport::Http(HttpSpec::new(endpoint.url("docs"), auth).unwrap())).unwrap();
    driver.app.ctx.services.mcp.add(spec).unwrap();
    driver
        .until("the declared document is listed", |app| {
            app.areas.surfaces.panel().available().iter().any(|a| a.info.uri == doc.info.uri && a.server == "deck")
        })
        .await;

    driver.surfaces(panel::Message::OpenAvailable(doc.info.uri.clone()));
    driver.until("it opens in a tab", |app| app.areas.surfaces.panel().active() == Some(&doc.info.uri)).await;
    let opened = driver.area().hub().opened(&doc.info.uri).unwrap();
    assert_eq!(opened.origin, Origin::Mcp("deck".into()));
    driver.app.ctx.services.mcp.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_get_the_surface_tools_as_host_tools_or_mcp_servers() {
    let dir = tempfile::tempdir().unwrap();
    let mut driver = Driver::new(dir.path(), hub(dir.path(), false));
    driver.until("the MCP endpoint runs", |app| app.areas.surfaces.endpoint.is_some()).await;
    let ctx = &driver.app.ctx;
    let area = driver.area();

    let mcp = Capabilities { mcp_servers: true, ..Capabilities::default() };
    let target = Target { harness: "x", capabilities: &mcp, resume: false };
    let mut config = SessionConfig::default();
    area.contribute(&mut config, &target, ctx);
    let names: Vec<&str> = config.mcp_servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["surface-browser", "surface-docs", "surface-vm"]);
    assert!(area.host_tools(&config, &target, ctx).is_none());

    let host = Capabilities { host_tools: true, mcp_servers: true, ..Capabilities::default() };
    let target = Target { harness: "x", capabilities: &host, resume: false };
    let mut config = SessionConfig::default();
    area.contribute(&mut config, &target, ctx);
    assert!(config.mcp_servers.is_empty(), "host tools win");
    let set = area.host_tools(&config, &target, ctx).unwrap();
    let names: Vec<&str> = set.specs.iter().map(|s| s.name.as_str()).collect();
    for tool in ["surface_list", "surface_screenshot", "surface_click", "surface_type", "surface_scroll"] {
        assert!(names.contains(&tool), "{names:?}");
    }
    // The Tools panel lists them as app tools.
    assert!(ctx.app_tools.iter().any(|tool| tool.name == "surface_screenshot"));
}

/// The installed omp in the app, on a real model: a browser surface opened in
/// the tab, then a session asked to screenshot it calls `surface_screenshot`
/// through the app's host tools. `LOA_LIVE_MODEL` picks the model.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp and Chromium with real model calls"]
async fn live_an_omp_session_in_the_app_screenshots_the_browser_surface() {
    use crate::services::{Dirs, Harnesses, Node, Services};
    use loa_identity::NodeIdentity;
    use loa_ui_composer as composer;

    let dir = tempfile::tempdir().unwrap();
    let node = Node::new(NodeIdentity::from_secret_bytes(&[9; 32]), "live".into());
    let dirs = Dirs::new(dir.path().join("loa"));
    let harnesses = Harnesses::detect(&node.key, &dirs.omp);
    let services = Services::new(node, dirs, harnesses, dir.path().to_owned()).unwrap();
    let (mut app, _) = App::build(services);
    app.ctx.sessions.harness = Some("omp".into());
    let hub = hub(dir.path(), true);
    assert!(hub.browser_executable().is_some(), "Chromium is installed (Playwright cache)");
    let mut driver = Driver::over(app, Arc::clone(&hub));

    let model = std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".into());
    let option = composer::ModelOption { id: model.clone(), name: model.clone(), provider: "anthropic".into(), role: None };
    driver.update(AppMessage::Models(crate::areas::models::Message::Model(option)));

    let page = "data:text/html,<title>Live</title><body style='margin:0;height:100vh;background:rgb(0,160,0)'><h1 style='color:white'>GREEN PAGE 4417</h1></body>";
    driver.surfaces(panel::Message::Url(page.into()));
    driver.surfaces(panel::Message::OpenUrl);
    driver.until("the tab is shown", |app| app.areas.surfaces.panel().active().is_some()).await;
    let uri = driver.area().panel().active().unwrap().clone();

    driver.update(AppMessage::NewSession);
    let started = async {
        while driver.app.ctx.sessions.active().and_then(|s| s.harness()).is_none() {
            let message = driver.messages.recv().await.unwrap();
            driver.update(message);
        }
    };
    tokio::time::timeout(Duration::from_secs(120), started).await.expect("omp starts");
    assert!(recorded(&driver.app).iter().any(|(u, status, _)| u == uri.as_str() && status == "open"));

    let text = format!(
        "Call the surface_screenshot tool with uri {uri}. Then answer with the main background color of the screenshot in one word."
    );
    for c in text.chars() {
        let edit = iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Insert(c));
        driver.update(AppMessage::Composer(composer::Message::Edit(edit)));
    }
    driver.update(AppMessage::Composer(composer::Message::Submit));

    // The turn ends with a successful surface_screenshot call.
    let done = |app: &App| {
        app.ctx.sessions.active().expect("session").writer().with_dom(|dom: &Dom| {
            let shot = dom.iter().find(|node| {
                node.kind == ElementKind::Call && node.str_attr(attr::TOOL).is_some_and(|tool| tool.ends_with("surface_screenshot"))
            });
            let ended = crate::session::dom_turns(dom)
                .last()
                .and_then(|turn| turn.str_attr(attr::STATUS))
                .is_some_and(|status| status == "done" || status == "error");
            shot.is_some() && ended
        })
    };
    // omp asks before every tool call: allow them, as the user would.
    let mut answered = std::collections::HashSet::new();
    let wait = async {
        while !done(&driver.app) {
            let session = driver.app.ctx.sessions.active().unwrap();
            let waiting: Vec<loa_core::NodeId> = session.writer().with_dom(|dom: &Dom| {
                dom.iter()
                    .filter(|node| node.kind == ElementKind::Decision && node.str_attr(attr::STATUS) == Some("waiting"))
                    .map(|node| node.id)
                    .collect()
            });
            for decision in waiting.into_iter().filter(|decision| answered.insert(*decision)) {
                let harness = Arc::clone(session.harness().unwrap());
                let answer = loa_adapter::Answer { values: vec![loa_core::answer::ALLOW_ONCE.into()], reason: None, by: "live".into() };
                tokio::spawn(async move { harness.answer(decision, answer).await });
            }
            tokio::select! {
                message = driver.messages.recv() => driver.update(message.unwrap()),
                () = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(240), wait).await.expect("the turn ends with a screenshot call");
    let (call, answer, failed) = driver.app.ctx.sessions.active().unwrap().writer().with_dom(|dom: &Dom| {
        let call = dom
            .iter()
            .find(|node| node.kind == ElementKind::Call && node.str_attr(attr::TOOL).is_some_and(|t| t.ends_with("surface_screenshot")))
            .unwrap();
        let failed = dom.children_of_kind(call.id, ElementKind::Result).any(|r| r.bool_attr(attr::IS_ERROR) == Some(true));
        let answer: String = dom
            .iter()
            .filter(|node| node.kind == ElementKind::Message && node.str_attr(attr::ROLE) == Some("assistant"))
            .map(|node| node.text.clone())
            .collect();
        (call.str_attr(attr::TOOL).unwrap().to_owned(), answer, failed)
    });
    eprintln!("call {call}; answer: {answer}");
    assert!(!failed, "the screenshot call succeeded");
    assert!(answer.to_lowercase().contains("green"), "{answer}");
    if let Some(harness) = driver.app.ctx.sessions.active().and_then(|s| s.harness()).cloned() {
        harness.shutdown().await.unwrap();
    }
    hub.shutdown().await;
}

/// The stream the tab of `uri` shows, as its header labels it.
fn stream_label(app: &App, uri: &SurfaceUri) -> Option<String> {
    app.areas.surfaces.panel().surface(uri).and_then(|s| s.stream()).map(|s| loa_ui_surface::stream_label(&s))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_tab_streams_video_with_the_named_encoder_and_the_toggle_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let hub = hub(dir.path(), true);
    if hub.browser_executable().is_none() {
        eprintln!("SKIPPED: no Chrome or Chromium installed");
        return;
    }
    let mut driver = Driver::new(dir.path(), Arc::clone(&hub));
    // The launcher learns what this machine encodes with.
    driver.until("the encoders were tried", |app| app.areas.surfaces.panel().video().encoder.is_some()).await;
    let video = driver.area().panel().video().clone();
    assert!(video.enabled);
    let encoder = video.encoder.clone().unwrap();
    assert_eq!(encoder.encoding, loa_surface::Encoding::H264);
    eprintln!("launcher: {} ({}), hardware encoders: {:?}", encoder.encoder, encoder.hardware, video.hardware);

    let page = "data:text/html,<body style='margin:0;height:100vh;background:rgb(255,0,0)' onmousedown=\"document.body.style.background='rgb(0,0,255)'\"></body>";
    driver.surfaces(panel::Message::Url(page.into()));
    driver.surfaces(panel::Message::OpenUrl);
    driver.until("the tab is shown", |app| app.areas.surfaces.panel().active().is_some()).await;
    let uri = driver.area().panel().active().unwrap().clone();
    let expected = loa_ui_surface::stream_label(&encoder);
    driver
        .until("the tab shows H.264 video", |app| {
            stream_label(app, &uri).as_deref() == Some(expected.as_str())
                && app.areas.surfaces.panel().surface(&uri).and_then(|s| s.shown_seq()).is_some()
        })
        .await;
    let red = |app: &App, blue: bool| {
        let Some(fb) = app.areas.surfaces.panel().surface(&uri).and_then(|s| s.framebuffer()) else { return false };
        let i = ((fb.height() / 2) as usize * fb.width() as usize + (fb.width() / 2) as usize) * 4;
        let px = &fb.pixels()[i..i + 3];
        if blue { px[2] > 180 && px[0] < 80 } else { px[0] > 180 && px[2] < 80 }
    };
    driver.until("the decoded video shows the red page", |app| red(app, false)).await;

    // The click comes back through the encoder and decoder.
    let size = Size::new(480.0, 320.0);
    let press = PointerInput::Button { at: Point::new(240.0, 160.0), button: MouseButton::Left, pressed: true };
    driver.surfaces(panel::Message::Surface(uri.clone(), loa_ui_surface::Message::Panel(size)));
    driver.surfaces(panel::Message::Surface(uri.clone(), loa_ui_surface::Message::Pointer { panel: size, input: press }));
    driver.until("the click shows in the video", |app| red(app, true)).await;
    assert_eq!(driver.area().panel().surface(&uri).unwrap().notice(), None);

    // Off: the same tab goes back to the JPEG screencast, and it is saved.
    driver.surfaces(panel::Message::Video(false));
    driver.until("the tab shows JPEG", |app| stream_label(app, &uri).as_deref() == Some("JPEG")).await;
    assert!(!hub.video());
    assert!(!driver.app.ctx.services.config.load().unwrap().config.surfaces.video);
    driver.surfaces(panel::Message::Video(true));
    driver.until("video is back", |app| stream_label(app, &uri).as_deref() == Some(expected.as_str())).await;
    assert!(driver.app.ctx.services.config.load().unwrap().config.surfaces.video);
    hub.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_virtual_display_capture_says_why_it_cant_run_and_is_saved_where_it_can() {
    let dir = tempfile::tempdir().unwrap();
    // No Xvfb: the launcher says so and the choice stays on the screencast.
    let mut config = HubConfig::discover(&dir.path().join("hub"));
    config.virtual_display = Err("Xvfb isn't installed (install the xvfb package)".into());
    let mut driver = Driver::new(dir.path(), SurfaceHub::new(config));
    let capture = driver.area().panel().capture().clone();
    assert_eq!(capture.virtual_display_missing.as_deref(), Some("Xvfb isn't installed (install the xvfb package)"));
    driver.surfaces(panel::Message::Capture(panel::Capture::VirtualDisplay));
    assert_eq!(driver.area().panel().capture().selected, panel::Capture::Screencast);
    assert_eq!(driver.area().hub().capture(), loa_surface_hub::CaptureMode::Screencast);

    // A fake Xvfb that reports a display but serves none: the choice is
    // taken and saved, and opening a tab fails with the browser's reason.
    let dir = tempfile::tempdir().unwrap();
    let mut config = HubConfig::discover(&dir.path().join("hub"));
    let Some(chrome) = config.browser.as_ref().map(|b| b.executable.clone()) else {
        eprintln!("SKIPPED: no Chrome or Chromium installed");
        return;
    };
    let xvfb = dir.path().join("Xvfb");
    // A display number no X server here listens on.
    let free = (6000..7000).find(|n| !std::path::Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()).unwrap();
    loa_exec::write_executable(&xvfb, format!("#!/bin/sh\necho {free}\nexec sleep 600\n")).unwrap();
    config.virtual_display = Ok(loa_surface_hub::VirtualDisplaySetup { xvfb, chrome });
    let hub = SurfaceHub::new(config);
    let mut driver = Driver::new(dir.path(), Arc::clone(&hub));
    assert_eq!(driver.area().panel().capture().virtual_display_missing, None);
    driver.surfaces(panel::Message::Capture(panel::Capture::VirtualDisplay));
    assert_eq!(hub.capture(), loa_surface_hub::CaptureMode::VirtualDisplay);
    let saved = driver.app.ctx.services.config.load().unwrap().config.surfaces;
    assert_eq!(saved.browser_capture, loa_config::BrowserCapture::VirtualDisplay);

    driver.surfaces(panel::Message::Url("about:blank".into()));
    driver.surfaces(panel::Message::OpenUrl);
    driver.until("opening fails on the launcher", |app| app.areas.surfaces.panel().error().is_some()).await;
    eprintln!("launcher error: {}", driver.area().panel().error().unwrap());
    assert_eq!(driver.area().panel().open_surfaces().count(), 0);
    hub.shutdown().await;
}

/// The tab's framebuffer pixel at `(x, y)`, RGB.
fn pixel(app: &App, uri: &SurfaceUri, x: u32, y: u32) -> Option<[u8; 3]> {
    let fb = app.areas.surfaces.panel().surface(uri).and_then(|s| s.framebuffer())?;
    let i = (y as usize * fb.width() as usize + x as usize) * 4;
    fb.pixels().get(i..i + 3).map(|px| [px[0], px[1], px[2]])
}

/// The center pixel of the tab's framebuffer, RGB.
fn center(app: &App, uri: &SurfaceUri) -> Option<[u8; 3]> {
    let fb = app.areas.surfaces.panel().surface(uri).and_then(|s| s.framebuffer())?;
    pixel(app, uri, fb.width() / 2, fb.height() / 2)
}

fn key(text: &str, pressed: bool) -> loa_ui_surface::Message {
    let code = format!("Key{}", text.to_uppercase());
    loa_ui_surface::Message::Key(loa_ui_surface::KeyInput {
        key: text.into(),
        code,
        pressed,
        modifiers: loa_surface::Modifiers::default(),
        text: pressed.then(|| text.into()),
    })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs a real Xvfb (from PATH) and headed Chromium"]
async fn live_a_browser_tab_on_a_real_virtual_display_streams_and_takes_clicks_and_typing() {
    let dir = tempfile::tempdir().unwrap();
    let hub = hub(dir.path(), true);
    if hub.browser_executable().is_none() {
        eprintln!("SKIPPED: no Chrome or Chromium installed");
        return;
    }
    let mut driver = Driver::new(dir.path(), Arc::clone(&hub));
    if let Some(missing) = driver.area().panel().capture().virtual_display_missing.clone() {
        eprintln!("SKIPPED: {missing}");
        return;
    }
    // The launcher switches capture to the virtual display.
    driver.surfaces(panel::Message::Capture(panel::Capture::VirtualDisplay));
    assert_eq!(driver.area().panel().capture().selected, panel::Capture::VirtualDisplay);
    assert_eq!(hub.capture(), loa_surface_hub::CaptureMode::VirtualDisplay);

    // Each click on the page flips it between red and blue; typing "ok" in
    // the field turns it green.
    let page = "data:text/html,<body style='margin:0;height:100vh;background:rgb(255,0,0)' \
        onmousedown=\"if(event.target==document.body)document.body.style.background=\
        document.body.style.background=='rgb(255, 0, 0)'?'rgb(0,0,255)':'rgb(255,0,0)'\">\
        <input id=f style='position:absolute;left:0;top:0;width:120px;height:30px' \
        oninput=\"if(this.value=='ok')document.body.style.background='rgb(0,255,0)'\"></body>";
    driver.surfaces(panel::Message::Url(page.into()));
    driver.surfaces(panel::Message::OpenUrl);
    driver
        .until("the tab is shown or failed", |app| {
            app.areas.surfaces.panel().active().is_some() || app.areas.surfaces.panel().error().is_some()
        })
        .await;
    assert_eq!(driver.area().panel().error(), None);
    let uri = driver.area().panel().active().unwrap().clone();
    let size = Size::new(480.0, 320.0);
    driver.surfaces(panel::Message::Surface(uri.clone(), loa_ui_surface::Message::Panel(size)));
    let is = |rgb: [u8; 3]| {
        let uri = uri.clone();
        move |app: &App| center(app, &uri).is_some_and(|px| px.iter().zip(rgb).all(|(&a, b)| a.abs_diff(b) < 60))
    };
    driver.until("the virtual display shows the red page", is([255, 0, 0])).await;
    // The page starts at the top of the picture: no browser bar sits above
    // it, so what the user sees lines up with where input lands.
    let top = pixel(&driver.app, &uri, 400, 2).unwrap();
    assert!(top[0] > 200 && top[1] < 60 && top[2] < 60, "top row is {top:?}, not the page");
    let stream = stream_label(&driver.app, &uri);
    eprintln!("stream: {stream:?}");

    // Five clicks, each waiting for its color, so the latency pill gets samples.
    for (i, (x, y)) in [(240.0, 160.0), (300.0, 200.0), (200.0, 250.0), (400.0, 100.0), (100.0, 300.0)]
        .into_iter()
        .enumerate()
    {
        for pressed in [true, false] {
            let input = PointerInput::Button { at: Point::new(x, y), button: MouseButton::Left, pressed };
            driver.surfaces(panel::Message::Surface(uri.clone(), loa_ui_surface::Message::Pointer { panel: size, input }));
        }
        let (what, rgb) = if i % 2 == 0 { ("blue", [0, 0, 255]) } else { ("red", [255, 0, 0]) };
        driver.until(&format!("click {} turned the page {what}", i + 1), is(rgb)).await;
    }
    driver
        .until("the latency pill has a reading", |app| app.areas.surfaces.panel().surface(&uri).and_then(|s| s.latency()).is_some())
        .await;

    // Click the field, then type "ok".
    let at = Point::new(40.0, 15.0);
    for pressed in [true, false] {
        let input = PointerInput::Button { at, button: MouseButton::Left, pressed };
        driver.surfaces(panel::Message::Surface(uri.clone(), loa_ui_surface::Message::Pointer { panel: size, input }));
    }
    assert!(driver.area().panel().surface(&uri).unwrap().focused());
    for c in ["o", "k"] {
        driver.surfaces(panel::Message::Surface(uri.clone(), key(c, true)));
        driver.surfaces(panel::Message::Surface(uri.clone(), key(c, false)));
    }
    driver.until("the typing turned the page green", is([0, 255, 0])).await;
    let reading = driver.area().panel().surface(&uri).unwrap().latency().unwrap();
    eprintln!("latency: {} ({} samples)", loa_ui_surface::latency_label(Some(reading)), reading.samples);
    assert!(reading.median_us > 0 && reading.p95_us >= reading.median_us && reading.p95_us < 1_000_000, "{reading:?}");
    assert_eq!(driver.area().panel().surface(&uri).unwrap().notice(), None);
    hub.shutdown().await;
}
