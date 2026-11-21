//! The hub with its real servers: documents from files, a VM over a fake RFB server, the surface
//! tools as host tools and behind the loopback MCP endpoint, and declared surfaces of an MCP
//! server opened as `McpSurface`s.

use bytes::Bytes;
use loa_contracts::harness::OutputBlock;
use loa_mcp::{AllowAll, HttpSpec, Manager, ServerSpec, Status, Transport};
use loa_contracts::surface::{Encoding, Frame, Input, InputEvent, Modifiers, MouseButton, SurfaceMode, SurfaceUri};
use loa_surfaces::docs::Fonts;
use loa_surfaces::pixels::RgbaImage;
use loa_surfaces::testkit::{PATIENCE, fake_rfb, next_frame};
use loa_surfaces::{HubConfig, HubEvent, McpEndpoint, Origin, SurfaceHub, SurfaceTools};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

fn config(dir: &Path, browser: bool) -> HubConfig {
    let mut config = HubConfig::discover(dir);
    config.fonts = Fonts::none();
    // Software H.264 only: the same on every machine, and no ffmpeg probing.
    #[cfg(feature = "remote-video")]
    {
        use loa_surfaces::proto::video::{LiveVideo, encode::probe};
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

fn shot_png(output: &loa_contracts::tools::ToolOutput) -> Option<&Bytes> {
    output.blocks.iter().find_map(|b| if let OutputBlock::Image { data, .. } = b { Some(data) } else { None })
}

fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    RgbaImage::filled(width, height, color).to_png().unwrap()
}

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
    let list = tools.run("surface_list", json!({}), None).await;
    assert!(!list.is_error);
    assert!(list.joined_text().contains(uri), "{list:?}");

    let shot = tools.run("surface_screenshot", json!({ "uri": uri }), None).await;
    let png_bytes = shot_png(&shot).expect("a screenshot image").clone();
    assert!(png_bytes.starts_with(b"\x89PNG"));
    let info = tools.run("surface_document_data", json!({ "uri": uri, "query": { "kind": "info" } }), None).await;
    assert!(info.joined_text().contains("\"format\": \"png\""), "{info:?}");
    let click = tools.run("surface_click", json!({ "uri": uri, "x": 1, "y": 1 }), None).await;
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
    let click = tools.run("surface_click", json!({ "uri": opened.info.uri.as_str(), "x": 3, "y": 2 }), Some("vm")).await;
    assert!(!click.is_error, "{click:?}");
    let pressed_at = [5u8, 1, 0, 3, 0, 2];
    tokio::time::timeout(PATIENCE, async {
        while !received.lock().unwrap().windows(6).any(|w| w == pressed_at) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the click reached the RFB server as a pointer event");
    let wrong_scope = tools.run("surface_screenshot", json!({ "uri": opened.info.uri.as_str() }), Some("browser")).await;
    assert!(wrong_scope.is_error);

    hub.close(&opened.info.uri).await.unwrap();
    // The endpoint stays listed for reconnecting.
    let listed = hub.list().await;
    assert_eq!((listed.len(), listed[0].open, &listed[0].origin), (1, false, &Origin::Vm));
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
    let shot = loa_mcp::toolkit::to_output(shot);
    assert!(shot_png(&shot).is_some_and(|png| png.starts_with(b"\x89PNG")), "{shot:?}");

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
    let shot = SurfaceTools::new(other.clone()).run("surface_screenshot", json!({ "uri": doc.info.uri.as_str() }), None).await;
    assert!(shot_png(&shot).is_some_and(|png| png.starts_with(b"\x89PNG")), "{shot:?}");
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
