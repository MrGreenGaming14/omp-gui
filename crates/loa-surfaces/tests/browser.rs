//! One smoke test against a real local Chrome, skipped with a loud message unless `LOA_CHROME`
//! is set.

use loa_contracts::surface::{Input, InputEvent, Modifiers, MouseButton, Surface, SurfaceError, SurfaceServer};
use loa_surfaces::browser::{BrowserConfig, BrowserError, BrowserServer};
use loa_surfaces::testkit::{html_data_url as html, next_frame};

const PAGE: &str = "<title>Smoke</title><style>body{margin:0} .abs{position:absolute} .gone{display:none}</style>
<script>console.log('script text must not show')</script>
<div class=gone>Hidden secret</div>
<button class=abs style='left:20px;top:30px;width:100px;height:40px' onclick=\"document.getElementById('out').textContent='saved ' + innerWidth\">Save</button>
<p class=abs id=out style='left:20px;top:150px'>idle</p>";

#[tokio::test(flavor = "multi_thread")]
async fn a_tab_streams_reads_takes_input_resizes_and_shuts_down() {
    let mut config = BrowserConfig::new(loa_surfaces::chrome_or_skip!());
    (config.sandbox, config.width, config.height) = (false, 640, 480);
    let server = BrowserServer::launch(config).await.expect("launch chrome");
    let tab = server.open_tab(&html(PAGE)).await.unwrap();
    let listed = server.list().await.unwrap();
    assert_eq!((listed[0].name.as_str(), listed[0].width, listed[0].height), ("Smoke", 640, 480));
    assert!(matches!(tab.document().await, Err(SurfaceError::Mode { .. })));

    let text = tab.page_text().await.unwrap();
    assert!(text.contains("idle") && !text.contains("script text") && !text.contains("Hidden secret"), "{text}");
    let outline = tab.outline().await.unwrap();
    let save = outline.iter().find(|n| n.role == "button" && n.name == "Save").expect("the Save button");
    assert_eq!((save.bounds.x, save.bounds.y, save.bounds.width, save.bounds.height), (20.0, 30.0, 100.0, 40.0));

    let mut frames = tab.frames().unwrap();
    let first = next_frame(&mut frames).await;
    tab.request_keyframe().await.unwrap();
    assert!(next_frame(&mut frames).await.seq > first.seq);

    let input = |seq, input| InputEvent { seq, sent_us: 0, input };
    tab.input(input(1, Input::Resize { width: 320, height: 240 })).await.unwrap();
    assert_eq!((tab.info().width, tab.info().height), (320, 240));
    let (x, y) = save.bounds.center();
    for (seq, pressed) in [(2, true), (3, false)] {
        let event = Input::MouseButton { x, y, button: MouseButton::Left, pressed, modifiers: Modifiers::default() };
        tab.input(input(seq, event)).await.unwrap();
    }
    assert!(tab.page_text().await.unwrap().contains("saved 320"));
    let resized = loop {
        let frame = next_frame(&mut frames).await;
        if frame.width == 320 {
            break frame;
        }
    };
    assert_eq!(resized.height, 240);
    let png = tab.screenshot().await.unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

    let pids = server.process_ids().await;
    server.shutdown().await.unwrap();
    assert!(!std::path::Path::new(&format!("/proc/{}", pids[0])).exists(), "chrome still running");
    assert!(matches!(server.open_tab("about:blank").await, Err(BrowserError::Closed)));
    assert!(server.list().await.unwrap().is_empty());
}
