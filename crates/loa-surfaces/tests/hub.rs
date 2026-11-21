//! The hub with its real servers: documents from files, a VM over a fake RFB server, the surface
//! tools as host tools and behind the loopback MCP endpoint, and declared surfaces of an MCP
//! server opened as `McpSurface`s.

use bytes::Bytes;
use loa_contracts::harness::OutputBlock;
use loa_contracts::surface::{Encoding, Input, InputEvent, Modifiers, MouseButton, SurfaceMode, SurfaceUri};
use loa_mcp::{AllowAll, HttpSpec, Manager, ServerSpec, Status, Transport};
use loa_surfaces::docs::Fonts;
use loa_surfaces::pixels::RgbaImage;
use loa_surfaces::testkit::{PATIENCE, fake_rfb, next_frame};
use loa_surfaces::{HubConfig, HubEvent, McpEndpoint, Origin, SurfaceHub, SurfaceTools};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// A hub without a browser that writes `name` as a `width` x `height` PNG of one colour.
fn new_hub(dir: &Path, name: &str, width: u32, height: u32, color: [u8; 4]) -> (Arc<SurfaceHub>, std::path::PathBuf) {
    let mut config = HubConfig::discover(dir);
    config.fonts = Fonts::none();
    config.browser = None;
    // Software H.264 only: the same on every machine, and no ffmpeg probing.
    #[cfg(feature = "remote-video")]
    {
        use loa_surfaces::proto::video::{LiveVideo, encode::probe};
        config.video.encoders = Some(Arc::new(LiveVideo::with_probed(None, probe(None))));
    }
    let image = dir.join(name);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(&image, RgbaImage::filled(width, height, color).to_png().unwrap()).unwrap();
    (SurfaceHub::new(config), image)
}

fn shot_png(output: &loa_contracts::tools::ToolOutput) -> Option<&Bytes> {
    output.blocks.iter().find_map(|b| if let OutputBlock::Image { data, .. } = b { Some(data) } else { None })
}

#[tokio::test(flavor = "multi_thread")]
async fn documents_and_vms_open_list_answer_tools_and_close() {
    let dir = tempfile::tempdir().unwrap();
    let (hub, image) = new_hub(dir.path(), "dot.png", 30, 20, [200, 10, 10, 255]);
    let mut events = hub.events();
    let tools = SurfaceTools::new(hub.clone());

    let opened = hub.open_document(&image).await.unwrap();
    assert_eq!(opened.origin, Origin::Document);
    assert_eq!((opened.info.mode, opened.info.width, opened.info.height), (SurfaceMode::Document, 30, 20));
    assert!(matches!(events.recv().await.unwrap(), HubEvent::Opened { origin: Origin::Document, .. }));
    let listed = hub.list().await;
    assert_eq!((listed.len(), listed[0].open), (1, true));
    let uri = opened.info.uri.as_str();
    let list = tools.run("surface_list", json!({}), None).await;
    assert!(!list.is_error && list.joined_text().contains(uri), "{list:?}");
    let shot = tools.run("surface_screenshot", json!({ "uri": uri }), None).await;
    let png_bytes = shot_png(&shot).expect("a screenshot image").clone();
    assert!(png_bytes.starts_with(b"\x89PNG"));
    let info = tools.run("surface_document_data", json!({ "uri": uri, "query": { "kind": "info" } }), None).await;
    assert!(info.joined_text().contains("\"format\": \"png\""), "{info:?}");
    assert!(tools.run("surface_click", json!({ "uri": uri, "x": 1, "y": 1 }), None).await.is_error, "documents take no clicks");

    // Host tools: text with a saved file, or the image itself.
    let (specs, handler) = tools.host_tools();
    assert!(specs.iter().any(|s| s.name == "surface_screenshot"));
    assert!(specs.iter().all(|s| serde_json::from_str::<serde_json::Value>(&s.input_schema).is_ok()));
    let args = json!({ "uri": uri }).to_string();
    let text = handler.call("s1", "surface_screenshot", &args).await.unwrap();
    let path = text.split(" saved to ").nth(1).expect("a saved path");
    assert!(text.starts_with("Screenshot (30x20)"), "{text}");
    assert_eq!(std::fs::read(path).unwrap(), png_bytes);
    let content = handler.call_content("s1", "surface_screenshot", &args).await.unwrap();
    assert!(matches!(&content[..], [OutputBlock::Image { mime, data }] if mime == "image/png" && *data == png_bytes));
    let missing = handler.call("s1", "surface_screenshot", r#"{"uri":"surface://docs/99"}"#).await.unwrap_err();
    assert!(missing.contains("no surface"), "{missing}");
    hub.close(&opened.info.uri).await.unwrap();
    assert!(hub.list().await.is_empty());
    assert!(matches!(events.recv().await.unwrap(), HubEvent::Closed { .. }));

    // A VM from host and port streams and takes agent clicks.
    assert!(hub.open_vm("no-port", None).await.is_err());
    let (address, received, send_update) = fake_rfb(8, 4, [0, 200, 0]).await;
    let vm = hub.open_vm(&address, None).await.unwrap();
    assert_eq!((&vm.origin, &vm.info.uri), (&Origin::Vm, &SurfaceUri::new("vm", &address).unwrap()));
    let mut frames = vm.surface.frames().unwrap();
    send_update.send(()).unwrap();
    let frame = loop {
        let frame = next_frame(&mut frames).await;
        if frame.data[..4] != [0, 0, 0, 255] {
            break frame;
        }
    };
    assert_eq!((frame.width, frame.height, frame.encoding, &frame.data[..4]), (8, 4, Encoding::Rgba, &[0, 200, 0, 255][..]));
    let vm_uri = vm.info.uri.as_str();
    let click = tools.run("surface_click", json!({ "uri": vm_uri, "x": 3, "y": 2 }), Some("vm")).await;
    assert!(!click.is_error, "{click:?}");
    let pressed_at = [5u8, 1, 0, 3, 0, 2];
    let reached = async {
        while !received.lock().unwrap().windows(6).any(|w| w == pressed_at) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(PATIENCE, reached).await.expect("the click reached the RFB server as a pointer event");
    assert!(tools.run("surface_screenshot", json!({ "uri": vm_uri }), Some("browser")).await.is_error);
    hub.close(&vm.info.uri).await.unwrap();
    // The endpoint stays listed for reconnecting.
    let listed = hub.list().await;
    assert_eq!((listed.len(), listed[0].open, &listed[0].origin), (1, false, &Origin::Vm));
}

fn http_spec(name: &str, url: String, token: &str) -> ServerSpec {
    let headers = vec![("Authorization".to_owned(), format!("Bearer {token}"))];
    ServerSpec::new(name, Transport::Http(HttpSpec::new(url, headers).unwrap())).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_mcp_endpoint_declares_surfaces_and_a_second_hub_opens_them() {
    let dir = tempfile::tempdir().unwrap();
    let (hub, image) = new_hub(dir.path(), "blue.png", 12, 6, [0, 0, 255, 255]);
    let doc = hub.open_document(&image).await.unwrap();
    let (address, _received, send_update) = fake_rfb(6, 3, [250, 250, 0]).await;
    let vm = hub.open_vm(&address, None).await.unwrap();
    send_update.send(()).unwrap();

    let endpoint = McpEndpoint::start(SurfaceTools::new(hub.clone())).await.unwrap();
    let names: Vec<_> = endpoint.specs().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["surface-browser", "surface-docs", "surface-vm"]);
    let manager = Manager::new(loa_mcp::Options::default()).unwrap();
    for server in ["docs", "vm"] {
        let name = format!("surface-{server}");
        manager.add(http_spec(&name, endpoint.url(server), endpoint.token())).unwrap();
        let mut status = manager.watch(&name).unwrap();
        let running = status.wait_for(|s| {
            if let Status::Error(e) = s {
                eprintln!("{name}: {e}");
            }
            matches!(s, Status::Running)
        });
        tokio::time::timeout(PATIENCE, running).await.expect("the MCP server runs").unwrap();
    }

    let docs_tools: Vec<String> = manager.tools("surface-docs", &AllowAll).unwrap().into_iter().map(|t| t.name.to_string()).collect();
    assert!(docs_tools.contains(&"surface_document_data".to_owned()) && !docs_tools.contains(&"surface_click".to_owned()), "{docs_tools:?}");
    let declared = manager.surfaces("surface-docs").await.unwrap();
    assert_eq!((declared.len(), &declared[0].0), (1, &doc.info.uri));
    let args = json!({ "uri": vm.info.uri.as_str() }).as_object().cloned();
    let shot = loa_mcp::toolkit::to_output(manager.call_tool("surface-vm", "surface_screenshot", args, &AllowAll).await.unwrap());
    assert!(shot_png(&shot).is_some_and(|png| png.starts_with(b"\x89PNG")), "{shot:?}");
    // Wrong token: refused.
    let intruder = Manager::new(loa_mcp::Options::default()).unwrap();
    assert!(intruder.probe(&http_spec("x", endpoint.url("vm"), "nope")).await.is_err(), "a bad token is refused");

    // A second hub sees them as third party MCP surfaces and opens them.
    let (other, _) = new_hub(&dir.path().join("other"), "unused.png", 1, 1, [0; 4]);
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

    // Document mode: the file comes over once through resources/read.
    let remote_doc = other.open(&doc.info.uri).await.unwrap();
    let document = remote_doc.surface.document().await.unwrap();
    assert_eq!((document.mime.as_str(), document.data.clone()), ("image/png", Bytes::from(std::fs::read(&image).unwrap())));
    assert!(remote_doc.surface.screenshot().await.unwrap().starts_with(b"\x89PNG"));
    let shot = SurfaceTools::new(other.clone()).run("surface_screenshot", json!({ "uri": doc.info.uri.as_str() }), None).await;
    assert!(shot_png(&shot).is_some_and(|png| png.starts_with(b"\x89PNG")), "{shot:?}");
    manager.shutdown().await;
}

#[cfg(not(target_os = "linux"))]
#[test]
fn the_virtual_display_says_it_needs_linux_elsewhere() {
    let error = loa_surfaces::VirtualDisplaySetup::find(None, None).unwrap_err();
    assert_eq!(error, "needs Linux (Xvfb)");
}
