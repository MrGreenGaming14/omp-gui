//! End to end tests against a real local Chrome. Each test is skipped with a
//! message when no browser is installed.

mod common;

use common::*;
use loa_surface::{Encoding, Input, Modifiers, MouseButton, Surface, SurfaceError, SurfaceMode, SurfaceServer, SurfaceUri};
use loa_surface_browser::{BrowserError, Tab, bench};
use std::sync::Arc;

/// Waits, one repaint at a time, until the page text contains `needle`.
async fn until_text(tab: &Tab, needle: &str) -> String {
    let mut frames = tab.frames().unwrap();
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let text = tab.page_text().await.unwrap();
        if text.contains(needle) {
            return text;
        }
        assert!(tokio::time::Instant::now() < deadline, "page text never contained {needle:?}: {text:?}");
        let _ = tokio::time::timeout_at(deadline, frames.recv()).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tabs_open_navigate_list_and_close() {
    let Some(server) = server(800, 600).await else { return };
    let base = serve(vec![
        ("/one", "<title>One</title><p>first page</p>"),
        ("/two", "<title>Two</title><p>second page</p>"),
    ])
    .await;

    let tab = server.open_tab(&format!("{base}/one")).await.unwrap();
    let uri = tab.uri().clone();
    assert_eq!(uri.server(), "browser");
    let info = tab.page_info().await.unwrap();
    assert_eq!((info.url.as_str(), info.title.as_str()), (format!("{base}/one").as_str(), "One"));

    let listed = server.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!((listed[0].name.as_str(), listed[0].width, listed[0].height), ("One", 800, 600));
    assert_eq!(listed[0].mode, SurfaceMode::Pixel);

    let surface = server.open(&uri).await.unwrap();
    assert!(matches!(surface.document().await, Err(SurfaceError::Mode { .. })));

    let second = server.open_tab(&format!("{base}/two")).await.unwrap();
    assert_ne!(second.uri(), &uri);
    let info = tab.navigate(&format!("{base}/two")).await.unwrap();
    assert_eq!(info.title, "Two");
    assert!(tab.page_text().await.unwrap().contains("second page"));

    server.close_tab(&uri).await.unwrap();
    let listed = server.list().await.unwrap();
    assert_eq!(listed.iter().map(|i| &i.uri).collect::<Vec<_>>(), vec![second.uri()]);
    assert!(matches!(server.open(&uri).await, Err(SurfaceError::NotFound(_))));
    let elsewhere: SurfaceUri = "surface://docs/tab1".parse().unwrap();
    assert!(matches!(server.open(&elsewhere).await, Err(SurfaceError::NotFound(_))));
    server.shutdown().await.unwrap();
}

const POINTER_PAGE: &str = "<title>p</title><style>html,body{margin:0;height:100%}</style><p id=out>none</p><script>
const log = [];
const out = document.getElementById('out');
const show = (s) => { log.push(s); out.textContent = log.join(' | '); };
addEventListener('mousedown', e => show(`down ${e.clientX},${e.clientY} b${e.button} d${e.detail}`));
addEventListener('click', e => show(`click d${e.detail}`));
addEventListener('dblclick', () => show('dblclick'));
addEventListener('contextmenu', e => { e.preventDefault(); show('contextmenu'); });
</script>";

#[tokio::test(flavor = "multi_thread")]
async fn mouse_clicks_land_at_surface_coordinates() {
    let Some(server) = server(640, 480).await else { return };
    let tab = server.open_tab(&html(POINTER_PAGE)).await.unwrap();
    let driver = Driver::new(tab.as_ref());

    driver.click(123.0, 45.0).await;
    let text = tab.page_text().await.unwrap();
    assert_eq!(text.trim(), "down 123,45 b0 d1 | click d1");

    driver.click(124.0, 45.0).await;
    let text = tab.page_text().await.unwrap();
    assert!(text.contains("down 124,45 b0 d2 | click d2 | dblclick"), "{text}");

    driver.button(300.0, 200.0, MouseButton::Right, true).await;
    driver.button(300.0, 200.0, MouseButton::Right, false).await;
    let text = tab.page_text().await.unwrap();
    assert!(text.ends_with("down 300,200 b2 d1 | contextmenu"), "{text}");
    server.shutdown().await.unwrap();
}

const FORM_PAGE: &str = "<title>f</title><input id=field autofocus style='position:absolute;left:10px;top:10px;width:300px;height:30px'>
<p id=out style='margin-top:80px'>-</p><script>
const field = document.getElementById('field');
field.addEventListener('keydown', e => { if (e.key === 'Enter') document.getElementById('out').textContent = 'submitted:' + field.value; });
</script>";

#[tokio::test(flavor = "multi_thread")]
async fn typing_text_and_editing_keys_reach_the_focused_field() {
    let Some(server) = server(640, 480).await else { return };
    let tab = server.open_tab(&html(FORM_PAGE)).await.unwrap();
    let driver = Driver::new(tab.as_ref());
    let none = Modifiers::default();

    driver.click(100.0, 25.0).await;
    driver.text("hello wörld").await;
    driver.key("Backspace", "Backspace", none).await;
    driver.key("ArrowLeft", "ArrowLeft", none).await;
    driver.text("X").await;
    driver.key("Enter", "Enter", none).await;
    assert!(tab.page_text().await.unwrap().contains("submitted:hello wörXl"));

    driver.key("a", "KeyA", Modifiers { ctrl: true, ..none }).await;
    driver.text("Z").await;
    driver.key("Enter", "Enter", none).await;
    assert!(tab.page_text().await.unwrap().contains("submitted:Z"));
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn wheel_scrolls_the_page_in_the_direction_asked() {
    let Some(server) = server(640, 480).await else { return };
    let page = "<title>s</title><div style='height:5000px'></div><p id=out style='position:fixed;top:0'>at 0</p><script>
addEventListener('scroll', () => document.getElementById('out').textContent = 'at ' + Math.round(scrollY));
</script>";
    let tab = server.open_tab(&html(page)).await.unwrap();
    let driver = Driver::new(tab.as_ref());
    driver.send(Input::Scroll { x: 100.0, y: 100.0, dx: 0.0, dy: 400.0, modifiers: Modifiers::default() }).await;
    until_text(&tab, "at 400").await;
    driver.send(Input::Scroll { x: 100.0, y: 100.0, dx: 0.0, dy: -150.0, modifiers: Modifiers::default() }).await;
    until_text(&tab, "at 250").await;
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn resize_changes_the_viewport_and_the_frame_size() {
    let Some(server) = server(800, 600).await else { return };
    let page = "<title>r</title><p id=out>-</p><script>
const show = () => document.getElementById('out').textContent = innerWidth + 'x' + innerHeight;
show(); addEventListener('resize', show);
</script>";
    let tab = server.open_tab(&html(page)).await.unwrap();
    assert!(tab.page_text().await.unwrap().contains("800x600"));
    let mut frames = tab.frames().unwrap();
    Driver::new(tab.as_ref()).send(Input::Resize { width: 640, height: 400 }).await;
    assert_eq!((tab.info().width, tab.info().height), (640, 400));

    let frame = loop {
        let frame = next_frame(&mut frames).await;
        if frame.width == 640 {
            break frame;
        }
    };
    assert_eq!((frame.width, frame.height), (640, 400));
    let decoded = decode(&frame);
    assert_eq!((decoded.0, decoded.1), (640, 400));
    until_text(&tab, "640x400").await;
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn screencast_frames_show_the_page_and_carry_the_last_handled_input() {
    let Some(server) = server(320, 240).await else { return };
    // Each press also bumps a counter, so every press repaints.
    let page = "<title>c</title><style>html,body{margin:0;height:100%;background:rgb(255,0,0)}</style><b id=n></b><script>
let n = 0;
addEventListener('mousedown', () => { document.body.style.background = 'rgb(0,255,0)'; document.getElementById('n').textContent = ++n; });
</script>";
    let tab = server.open_tab(&html(page)).await.unwrap();
    let mut frames = tab.frames().unwrap();
    // Chrome may paint once at its window size before the viewport override
    // lands; such frames are smaller and skipped here.
    let red = loop {
        let frame = next_frame(&mut frames).await;
        if (frame.width, frame.height) == (320, 240) && close_to(pixel(&decode(&frame), 160, 120), [255, 0, 0]) {
            break frame;
        }
    };
    assert_eq!((red.encoding, red.keyframe, red.input_seq), (Encoding::Jpeg, true, 0));
    assert_eq!((red.region.width, red.region.height), (320, 240));

    let driver = Driver::new(tab.as_ref());
    driver.send(Input::MouseMove { x: 10.0, y: 10.0 }).await;
    let pressed = driver.button(10.0, 10.0, MouseButton::Left, true).await;
    let mut last_seq = red.seq;
    let green = loop {
        let frame = next_frame(&mut frames).await;
        assert!(frame.seq > last_seq, "frame seqs count up");
        last_seq = frame.seq;
        if (frame.width, frame.height) == (320, 240) && close_to(pixel(&decode(&frame), 160, 120), [0, 255, 0]) {
            break frame;
        }
        assert!(frame.input_seq < pressed, "frame {} claims input {} but is still red", frame.seq, frame.input_seq);
    };
    assert!(green.input_seq <= pressed);
    // A frame painted after the press was handled carries its seq. Chrome
    // may have painted the green frame just before the acknowledgement
    // reached us; then the repaint of a later press carries it. A resent
    // keyframe of the old picture never claims inputs it doesn't show.
    if green.input_seq < pressed {
        driver.button(10.0, 10.0, MouseButton::Left, false).await;
        let again = driver.button(10.0, 10.0, MouseButton::Left, true).await;
        loop {
            let next = next_frame(&mut frames).await;
            assert!(next.input_seq <= again);
            if next.input_seq >= pressed {
                break;
            }
        }
    }
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_late_viewer_gets_the_last_frame_on_request() {
    let Some(server) = server(320, 240).await else { return };
    let tab = server.open_tab(&html("<title>k</title><p>static</p>")).await.unwrap();
    let mut early = tab.frames().unwrap();
    // Chrome may paint once before the viewport override takes effect; the
    // late viewer is asked for the last frame after the page settles.
    let first = loop {
        let frame = next_frame(&mut early).await;
        if (frame.width, frame.height) == (320, 240) {
            break frame;
        }
    };
    let mut late = tab.frames().unwrap();
    tab.request_keyframe().await.unwrap();
    let again = next_frame(&mut late).await;
    assert!(again.keyframe && again.seq > first.seq);
    let decoded = decode(&again);
    assert_eq!((decoded.0, decoded.1), (320, 240));
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn screenshot_is_a_png_of_the_surface() {
    let Some(server) = server(500, 300).await else { return };
    let tab = server.open_tab(&html("<title>s</title><p>shot</p>")).await.unwrap();
    let png = tab.screenshot().await.unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((width, height), (500, 300));
    server.shutdown().await.unwrap();
}

const OUTLINE_PAGE: &str = "<title>Outline test</title>
<style>body{margin:0} .abs{position:absolute} .gone{display:none}</style>
<script>console.log('script text must not show')</script>
<p>Visible paragraph</p>
<div class=gone>Hidden secret</div>
<button class=abs id=save style='left:20px;top:30px;width:100px;height:40px' onclick=\"document.getElementById('status').textContent='saved'\">Save</button>
<a class=abs href='#docs' style='left:200px;top:30px'>Docs</a>
<input class=abs aria-label=Search style='left:20px;top:100px;width:200px;height:24px'>
<div class=abs role=button tabindex=0 style='left:300px;top:100px;width:50px;height:50px'>Custom</div>
<p class=abs id=status style='left:20px;top:150px'>idle</p>
<button class=abs style='left:20px;top:2000px;width:80px;height:20px'>Far</button>";

#[tokio::test(flavor = "multi_thread")]
async fn structured_reads_give_url_title_text_and_clickable_outline() {
    let Some(server) = server(640, 480).await else { return };
    let url = html(OUTLINE_PAGE);
    let tab = server.open_tab(&url).await.unwrap();

    let info = tab.page_info().await.unwrap();
    assert_eq!((info.url.as_str(), info.title.as_str()), (url.as_str(), "Outline test"));
    assert_eq!(tab.info().name, "Outline test");

    let text = tab.page_text().await.unwrap();
    assert!(text.contains("Visible paragraph") && text.contains("idle"), "{text}");
    for absent in ["script text", "Hidden secret", "position:absolute", "<p>"] {
        assert!(!text.contains(absent), "{absent:?} leaked into {text:?}");
    }

    let outline = tab.outline().await.unwrap();
    let find = |role: &str, name: &str| {
        outline
            .iter()
            .find(|n| n.role == role && n.name == name)
            .unwrap_or_else(|| panic!("no {role} {name:?} in {outline:#?}"))
    };
    let save = find("button", "Save");
    assert_eq!((save.bounds.x, save.bounds.y, save.bounds.width, save.bounds.height), (20.0, 30.0, 100.0, 40.0));
    assert!(save.visible);
    let docs = find("link", "Docs");
    assert_eq!((docs.bounds.x, docs.bounds.y), (200.0, 30.0));
    let search = find("textbox", "Search");
    assert_eq!((search.bounds.x, search.bounds.y), (20.0, 100.0));
    find("button", "Custom");
    let far = find("button", "Far");
    assert!(!far.visible);
    assert!(outline.iter().all(|n| n.name != "Hidden secret"));

    let (x, y) = save.bounds.center();
    Driver::new(tab.as_ref()).click(x, y).await;
    assert!(tab.page_text().await.unwrap().contains("saved"));

    // Boxes follow the scroll position: surface coordinates, not page ones.
    let driver = Driver::new(tab.as_ref());
    driver.send(Input::Scroll { x: 10.0, y: 10.0, dx: 0.0, dy: 1700.0, modifiers: Modifiers::default() }).await;
    let mut frames = tab.frames().unwrap();
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let outline = tab.outline().await.unwrap();
        let far = outline.iter().find(|n| n.name == "Far").unwrap();
        if far.visible {
            assert_eq!(far.bounds.y, 460.0, "scroll stops at the document's last row");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "never scrolled: {far:?}");
        let _ = tokio::time::timeout_at(deadline, frames.recv()).await;
    }
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_kills_chrome_and_refuses_new_tabs() {
    let Some(server) = server(320, 240).await else { return };
    let tab = server.open_tab(&html("<p>x</p>")).await.unwrap();
    let pids = server.process_ids().await;
    assert_eq!(pids.len(), 1);
    assert!(std::path::Path::new(&format!("/proc/{}", pids[0])).exists());
    server.shutdown().await.unwrap();
    assert!(!std::path::Path::new(&format!("/proc/{}", pids[0])).exists(), "chrome still running");
    assert!(matches!(server.open_tab("about:blank").await, Err(BrowserError::Closed)));
    assert!(server.list().await.unwrap().is_empty());
    assert!(tab.page_text().await.is_err());
}

/// The benchmark the design README asks for. Prints the numbers; run with
/// `--nocapture` to see them.
#[tokio::test(flavor = "multi_thread")]
async fn screencast_click_latency_benchmark() {
    let Some(server) = server(1280, 720).await else { return };
    let report = bench::click_latency(&server, 100).await.unwrap();
    println!("screencast click to frame latency: {report}");
    assert_eq!(report.samples.len(), 100);
    assert_eq!(report.overclaimed, 0, "frames claimed inputs they did not show");
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn server_is_usable_as_a_shared_surface_server() {
    let Some(server) = server(320, 240).await else { return };
    let server: Arc<dyn SurfaceServer> = Arc::new(server);
    assert_eq!(server.name(), "browser");
    assert!(server.list().await.unwrap().is_empty());
}
