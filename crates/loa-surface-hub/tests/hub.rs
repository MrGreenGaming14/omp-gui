//! The hub with its real servers: documents from files, a VM over a fake RFB
//! server, the browser on the installed Chrome (skipped without one), the
//! surface tools as host tools and behind the loopback MCP endpoint, and
//! declared surfaces of an MCP server opened as `McpSurface`s.

use bytes::Bytes;
use loa_contracts::harness::OutputBlock;
use loa_mcp::{AllowAll, HttpSpec, Manager, ServerSpec, Status, Transport};
use loa_contracts::surface::{Encoding, Frame, Input, InputEvent, Modifiers, MouseButton, SurfaceMode, SurfaceUri};
use loa_surface_docs::Fonts;
use loa_surface_hub::{
    CaptureMode, HubConfig, HubEvent, McpEndpoint, Origin, SurfaceHub, SurfaceTools, ToolContent,
    VirtualDisplaySetup,
};
use serde_json::json;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, oneshot};

const PATIENCE: Duration = Duration::from_secs(20);

fn config(dir: &Path, browser: bool) -> HubConfig {
    let mut config = HubConfig::discover(dir);
    config.fonts = Fonts::none();
    // Software H.264 only: the same on every machine, and no ffmpeg probing.
    #[cfg(feature = "remote-video")]
    {
        use loa_surface_proto::video::{LiveVideo, encode::probe};
        config.video.encoders = Some(Arc::new(LiveVideo::with_probed(None, probe(None))));
    }
    if browser {
        if let Some(b) = &mut config.browser {
            b.width = 480;
            b.height = 320;
        }
    } else {
        config.browser = None;
    }
    config
}

fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(width, height, image::Rgba(color)).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

async fn next_frame(frames: &mut broadcast::Receiver<Arc<Frame>>) -> Arc<Frame> {
    loop {
        match tokio::time::timeout(PATIENCE, frames.recv()).await.expect("a frame in time") {
            Ok(frame) => return frame,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(e) => panic!("frame stream: {e}"),
        }
    }
}

/// The first frame showing the fake RFB server's update. A new subscriber is
/// first sent the current picture, which is still the blank framebuffer when
/// its keyframe is taken before the update arrived.
async fn updated_frame(frames: &mut broadcast::Receiver<Arc<Frame>>, encoding: Encoding) -> Arc<Frame> {
    loop {
        let frame = frame_of(frames, encoding).await;
        if frame.data[..4] != [0, 0, 0, 255] {
            return frame;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn documents_open_list_answer_tools_and_close() {
    let dir = tempfile::tempdir().unwrap();
    let hub = SurfaceHub::new(config(dir.path(), false));
    let mut events = hub.events();
    let image = dir.path().join("dot.png");
    std::fs::write(&image, png(30, 20, [200, 10, 10, 255])).unwrap();

    let opened = hub.open_document(&image).await.unwrap();
    assert_eq!(opened.origin, Origin::Document);
    assert_eq!((opened.info.mode, opened.info.width, opened.info.height), (SurfaceMode::Document, 30, 20));
    assert!(matches!(events.recv().await.unwrap(), HubEvent::Opened { origin: Origin::Document, .. }));
    let listed = hub.list().await;
    assert_eq!(listed.len(), 1);
    assert!(listed[0].open);

    let tools = SurfaceTools::new(hub.clone());
    let uri = opened.info.uri.as_str();
    let list = tools.run("surface_list", &json!({}), None).await;
    assert!(!list.is_error);
    assert!(list.joined_text().contains(uri), "{list:?}");

    let shot = tools.run("surface_screenshot", &json!({ "uri": uri }), None).await;
    let png_bytes = shot.image().expect("a screenshot image").clone();
    assert!(png_bytes.starts_with(b"\x89PNG"));
    let info = tools.run("surface_document_data", &json!({ "uri": uri, "query": { "kind": "info" } }), None).await;
    assert!(info.joined_text().contains("\"format\": \"png\""), "{info:?}");
    let click = tools.run("surface_click", &json!({ "uri": uri, "x": 1, "y": 1 }), None).await;
    assert!(click.is_error, "documents take no clicks");

    // Host tools: text with a saved file, or the image itself.
    let (specs, handler) = tools.host_tools();
    assert!(specs.iter().any(|s| s.name == "surface_screenshot"));
    assert!(specs.iter().all(|s| serde_json::from_str::<serde_json::Value>(&s.input_schema).is_ok()));
    let text = handler.call("s1", "surface_screenshot", &json!({ "uri": uri }).to_string()).await.unwrap();
    let path = text.split(" saved to ").nth(1).expect("a saved path");
    assert!(text.starts_with("Screenshot (30x20)"), "{text}");
    assert_eq!(std::fs::read(path).unwrap(), png_bytes);
    let content = handler.call_content("s1", "surface_screenshot", &json!({ "uri": uri }).to_string()).await.unwrap();
    assert!(matches!(&content[..], [OutputBlock::Image { mime, data }] if mime == "image/png" && *data == png_bytes));
    let missing = handler.call("s1", "surface_screenshot", r#"{"uri":"surface://docs/99"}"#).await.unwrap_err();
    assert!(missing.contains("no surface"), "{missing}");

    hub.close(&opened.info.uri).await.unwrap();
    assert!(hub.list().await.is_empty());
    assert!(matches!(events.recv().await.unwrap(), HubEvent::Closed { .. }));
}

/// A one-connection RFB 3.8 server: no security, `width` x `height`, one
/// solid raw update after the client's setup once the returned sender fires.
/// Frames go to whoever is subscribed when they are published, so a test
/// subscribes first and then lets the update go. Collects what the client
/// sends after the setup.
async fn fake_rfb(width: u16, height: u16, color: [u8; 3]) -> (String, Arc<Mutex<Vec<u8>>>, oneshot::Sender<()>) {
    let (send_update, update_sent) = oneshot::channel();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let received = Arc::new(Mutex::new(Vec::new()));
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
        if update_sent.await.is_err() {
            return;
        }
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
    (address, received, send_update)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_vm_opens_from_host_and_port_streams_and_takes_agent_clicks() {
    let dir = tempfile::tempdir().unwrap();
    let hub = SurfaceHub::new(config(dir.path(), false));
    assert!(hub.open_vm("no-port", None).await.is_err());
    let (address, received, send_update) = fake_rfb(8, 4, [0, 200, 0]).await;

    let opened = hub.open_vm(&address, None).await.unwrap();
    assert_eq!(opened.origin, Origin::Vm);
    assert_eq!(opened.info.uri, SurfaceUri::new("vm", &address).unwrap());
    let mut frames = opened.surface.frames().unwrap();
    send_update.send(()).unwrap();
    let frame = updated_frame(&mut frames, Encoding::Rgba).await;
    assert_eq!((frame.width, frame.height, frame.encoding), (8, 4, Encoding::Rgba));
    assert_eq!(&frame.data[..4], &[0, 200, 0, 255]);

    let tools = SurfaceTools::new(hub.clone());
    let click = tools.run("surface_click", &json!({ "uri": opened.info.uri.as_str(), "x": 3, "y": 2 }), Some("vm")).await;
    assert!(!click.is_error, "{click:?}");
    let pressed_at = [5u8, 1, 0, 3, 0, 2];
    tokio::time::timeout(PATIENCE, async {
        while !received.lock().unwrap().windows(6).any(|w| w == pressed_at) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the click reached the RFB server as a pointer event");
    let wrong_scope = tools.run("surface_screenshot", &json!({ "uri": opened.info.uri.as_str() }), Some("browser")).await;
    assert!(wrong_scope.is_error);

    hub.close(&opened.info.uri).await.unwrap();
    // The endpoint stays listed for reconnecting.
    let listed = hub.list().await;
    assert_eq!((listed.len(), listed[0].open, &listed[0].origin), (1, false, &Origin::Vm));
}

fn html(body: &str) -> String {
    format!("data:text/html,{}", loa_util::text::percent_encode(body))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_tab_streams_and_the_agent_drives_and_reads_it() {
    let dir = tempfile::tempdir().unwrap();
    let hub = SurfaceHub::new(config(dir.path(), true));
    let Some(chrome) = hub.browser_executable() else {
        eprintln!("SKIPPED: no Chrome or Chromium installed");
        return;
    };
    eprintln!("browser: {}", chrome.display());
    let page = "<title>Hub</title><body style='margin:0;height:100vh;background:rgb(255,0,0)'>\
        <input id=q style='width:200px'><script>addEventListener('mousedown',()=>document.body.style.background='rgb(0,0,255)')</script></body>";
    let opened = hub.open_browser(&html(page)).await.unwrap();
    assert_eq!(opened.origin, Origin::Browser);
    assert_eq!((opened.info.width, opened.info.height), (480, 320));
    let mut frames = opened.surface.frames().unwrap();
    let first = next_frame(&mut frames).await;
    assert_eq!(first.encoding, Encoding::Jpeg);

    let uri = opened.info.uri.as_str();
    let tools = SurfaceTools::new(hub.clone());
    // The user's renderer is one input source, the agent another.
    opened
        .surface
        .input(InputEvent {
            seq: 1,
            sent_us: 0,
            input: Input::MouseButton { x: 400.0, y: 300.0, button: MouseButton::Left, pressed: true, modifiers: Modifiers::default() },
        })
        .await
        .unwrap();
    let text = tools.run("surface_page_text", &json!({ "uri": uri }), None).await;
    assert!(text.joined_text().contains("Title: Hub"), "{text:?}");
    let click = tools.run("surface_click", &json!({ "uri": uri, "x": 20, "y": 10 }), None).await;
    assert!(!click.is_error, "{click:?}");
    let typed = tools.run("surface_type", &json!({ "uri": uri, "text": "hello" }), None).await;
    assert!(!typed.is_error, "{typed:?}");
    let shot = tools.run("surface_screenshot", &json!({ "uri": uri }), None).await;
    let png_bytes = shot.image().expect("a screenshot");
    let decoded = image::load_from_memory(png_bytes).unwrap().to_rgba8();
    assert_eq!(decoded.get_pixel(400, 300).0[..3], [0, 0, 255], "the page saw the user's press");
    let elements = tools.run("surface_elements", &json!({ "uri": uri }), None).await;
    assert!(elements.joined_text().contains("textbox"), "{elements:?}");
    let navigated = tools.run("surface_navigate", &json!({ "uri": uri, "url": html("<title>Two</title>two") }), None).await;
    assert!(navigated.joined_text().contains("Two"), "{navigated:?}");

    // The agent can open its own tab; the hub announces it.
    let mut events = hub.events();
    let own = tools.run("surface_open_browser", &json!({ "url": "about:blank" }), None).await;
    assert!(own.joined_text().contains("surface://browser/tab2"), "{own:?}");
    assert!(matches!(events.recv().await.unwrap(), HubEvent::Opened { origin: Origin::Browser, .. }));
    assert_eq!(hub.open_surfaces().len(), 2);
    hub.shutdown().await;
}

async fn wait_running(manager: &Manager, name: &str) {
    let mut status = manager.watch(name).unwrap();
    tokio::time::timeout(PATIENCE, async {
        loop {
            let now = status.borrow_and_update().clone();
            match now {
                Status::Running => return,
                Status::Error(e) => eprintln!("{name}: {e}"),
                _ => {}
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .expect("the MCP server runs");
}

fn http_spec(name: &str, url: String, headers: Vec<(String, String)>) -> ServerSpec {
    ServerSpec::new(name, Transport::Http(HttpSpec::new(url, headers).unwrap())).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_mcp_endpoint_declares_surfaces_and_a_second_hub_opens_them() {
    let dir = tempfile::tempdir().unwrap();
    let hub = SurfaceHub::new(config(dir.path(), false));
    let image = dir.path().join("blue.png");
    std::fs::write(&image, png(12, 6, [0, 0, 255, 255])).unwrap();
    let doc = hub.open_document(&image).await.unwrap();
    let (address, _received, send_update) = fake_rfb(6, 3, [250, 250, 0]).await;
    let vm = hub.open_vm(&address, None).await.unwrap();
    send_update.send(()).unwrap();

    let endpoint = McpEndpoint::start(SurfaceTools::new(hub.clone())).await.unwrap();
    let specs = endpoint.specs();
    assert_eq!(specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["surface-browser", "surface-docs", "surface-vm"]);

    let manager = Manager::new(loa_mcp::Options::default()).unwrap();
    let auth = vec![("Authorization".to_owned(), format!("Bearer {}", endpoint.token()))];
    manager.add(http_spec("surface-docs", endpoint.url("docs"), auth.clone())).unwrap();
    manager.add(http_spec("surface-vm", endpoint.url("vm"), auth.clone())).unwrap();
    wait_running(&manager, "surface-docs").await;
    wait_running(&manager, "surface-vm").await;

    let docs_tools: Vec<String> = manager.tools("surface-docs", &AllowAll).unwrap().into_iter().map(|t| t.name.to_string()).collect();
    assert!(docs_tools.contains(&"surface_document_data".to_owned()) && !docs_tools.contains(&"surface_click".to_owned()), "{docs_tools:?}");
    let declared = manager.surfaces("surface-docs").await.unwrap();
    assert_eq!(declared.len(), 1);
    assert_eq!(declared[0].0, doc.info.uri);
    let mut args = serde_json::Map::new();
    args.insert("uri".into(), json!(vm.info.uri.as_str()));
    let shot = manager.call_tool("surface-vm", "surface_screenshot", Some(args), &AllowAll).await.unwrap();
    let shot = loa_surface_hub::ToolResult::from_mcp(&serde_json::to_value(&shot).unwrap());
    assert!(shot.image().is_some_and(|png| png.starts_with(b"\x89PNG")), "{shot:?}");

    // Wrong token: refused.
    let intruder = Manager::new(loa_mcp::Options::default()).unwrap();
    let probe = intruder.probe(&http_spec("x", endpoint.url("vm"), vec![("Authorization".into(), "Bearer nope".into())])).await;
    assert!(probe.is_err(), "a bad token is refused");

    // A second hub sees them as third party MCP surfaces and opens them.
    let other = SurfaceHub::new(config(&dir.path().join("other"), false));
    assert!(other.refresh_declared(&manager).await.is_empty());
    let listed = other.list().await;
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert!(listed.iter().all(|l| !l.open && matches!(l.origin, Origin::Mcp(_))));

    let remote_vm = other.open(&vm.info.uri).await.unwrap();
    assert_eq!(remote_vm.origin, Origin::Mcp("surface-vm".into()));
    let mut frames = remote_vm.surface.frames().unwrap();
    let frame = next_frame(&mut frames).await;
    assert_eq!((frame.encoding, frame.width, frame.height), (Encoding::Png, 6, 3));
    // A click through the MCP surface reaches the RFB server and stamps the next frame.
    for pressed in [true, false] {
        let input = Input::MouseButton { x: 1.0, y: 1.0, button: MouseButton::Left, pressed, modifiers: Modifiers::default() };
        remote_vm.surface.input(InputEvent { seq: 7, sent_us: 0, input }).await.unwrap();
    }
    let stamped = loop {
        let frame = next_frame(&mut frames).await;
        if frame.input_seq == 7 {
            break frame;
        }
    };
    assert!(stamped.keyframe);

    let remote_doc = other.open(&doc.info.uri).await.unwrap();
    // Document mode: the file comes over once through resources/read.
    let document = remote_doc.surface.document().await.unwrap();
    assert_eq!((document.mime.as_str(), document.data.clone()), ("image/png", Bytes::from(std::fs::read(&image).unwrap())));
    let picture = remote_doc.surface.screenshot().await.unwrap();
    assert!(picture.starts_with(b"\x89PNG"));
    match &SurfaceTools::new(other.clone()).run("surface_screenshot", &json!({ "uri": doc.info.uri.as_str() }), None).await.content[0] {
        ToolContent::Image { data, .. } => assert!(data.starts_with(b"\x89PNG")),
        other => panic!("{other:?}"),
    }
    manager.shutdown().await;
}

async fn frame_of(frames: &mut broadcast::Receiver<Arc<Frame>>, encoding: Encoding) -> Arc<Frame> {
    loop {
        let frame = next_frame(frames).await;
        if frame.encoding == encoding {
            return frame;
        }
    }
}

#[cfg(feature = "remote-video")]
#[tokio::test(flavor = "multi_thread")]
async fn a_vm_streams_h264_to_a_network_viewer_raw_frames_here_and_the_toggle_turns_video_off() {
    let dir = tempfile::tempdir().unwrap();
    let hub = SurfaceHub::new(config(dir.path(), false));
    assert!(hub.video().enabled());
    let (address, _received, send_update) = fake_rfb(8, 4, [0, 200, 0]).await;
    let opened = hub.open_vm(&address, None).await.unwrap();
    let mut frames = opened.surface.frames().unwrap();
    send_update.send(()).unwrap();
    assert_eq!(&updated_frame(&mut frames, Encoding::Rgba).await.data[..4], &[0, 200, 0, 255]);

    // A viewer across the network that decodes H.264 gets video...
    let network = hub.network_surface(&opened.info.uri).await.unwrap();
    let mut coded = network.frames().unwrap();
    let stream = network.negotiate(&[Encoding::H264, Encoding::Rgba]).await.unwrap().unwrap();
    assert_eq!((stream.encoding, stream.encoder.as_str()), (Encoding::H264, "rusty_h264"));
    let frame = frame_of(&mut coded, Encoding::H264).await;
    assert!(frame.keyframe);
    let mut picture = vec![0; 8 * 4 * 4];
    assert!(loa_surface_proto::video::decode::VideoDecoder::new().decode(&frame, &mut picture).unwrap());
    let [r, g, b, _] = picture[..4] else { unreachable!() };
    assert!(r < 40 && g > 160 && b < 40, "decoded {:?}", &picture[..4]);
    assert!(Arc::ptr_eq(&network, &hub.network_surface(&opened.info.uri).await.unwrap()), "one encoder per surface");

    // ...while the viewer in this process keeps the source frames.
    opened.surface.request_keyframe().await.unwrap();
    let frame = next_frame(&mut frames).await;
    assert_eq!((frame.encoding, &frame.data[..4]), (Encoding::Rgba, &[0, 200, 0, 255][..]));

    hub.video().set_enabled(false).await;
    assert_eq!(network.stream().unwrap().encoder, loa_surface_proto::video::SOURCE);
    let frame = frame_of(&mut coded, Encoding::Rgba).await;
    assert_eq!(&frame.data[..4], &[0, 200, 0, 255]);
}

fn fake_xvfb(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("Xvfb");
    // Reports a display like `-displayfd 1` asks, then serves nothing. The
    // number is one no X server on this machine listens on (a fixed one
    // can belong to a real server, which Chrome would then draw on).
    let free = (6000..7000).find(|n| !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()).unwrap();
    loa_util::exec::write_executable(&path, format!("#!/bin/sh\necho {free}\nexec sleep 600\n")).unwrap();
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn the_virtual_display_capture_is_refused_where_it_cant_run_and_tried_where_it_can() {
    let dir = tempfile::tempdir().unwrap();
    let mut missing = config(dir.path(), true);
    missing.virtual_display = Err("Xvfb isn't installed (install the xvfb package)".into());
    let hub = SurfaceHub::new(missing);
    let error = hub.set_capture(CaptureMode::VirtualDisplay).unwrap_err();
    assert!(error.to_string().contains("Xvfb isn't installed"), "{error}");
    assert_eq!(hub.capture(), CaptureMode::Screencast);

    // A display that reports a number but accepts no X clients: the hub
    // picks the capture and says plainly why the tab can't open.
    let mut fake = config(dir.path(), true);
    let Some(chrome) = fake.browser.as_ref().map(|b| b.executable.clone()) else {
        eprintln!("SKIPPED: no Chrome or Chromium installed");
        return;
    };
    fake.virtual_display = Ok(VirtualDisplaySetup { xvfb: fake_xvfb(dir.path()), chrome });
    let hub = SurfaceHub::new(fake);
    hub.set_capture(CaptureMode::VirtualDisplay).unwrap();
    assert_eq!(hub.capture(), CaptureMode::VirtualDisplay);
    let error = tokio::time::timeout(Duration::from_secs(60), hub.open_browser("about:blank"))
        .await
        .expect("fails in time")
        .unwrap_err();
    eprintln!("virtual display tab: {error}");
    assert!(hub.open_surfaces().is_empty());
    assert!(!hub.capture_pending().await);
}
