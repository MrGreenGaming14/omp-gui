//! A real browser surface (installed Chrome or Chromium, skipped without
//! one) in the Surfaces tab: frames stream into the renderer, a click on the
//! panel reaches the page, and the latency meter pairs it with the frame
//! that shows it.

use futures::StreamExt;
use iced::{Point, Size, Task};
use iced_runtime::Action;
use loa_surface::MouseButton;
use loa_surface_hub::{HubConfig, SurfaceHub};
use loa_ui_surface::panel::Message;
use loa_ui_surface::{BrowserStatus, Message as SurfaceMessage, Panel, PanelOptions, PointerInput};
use std::time::Duration;
use tokio::sync::mpsc;

fn spawn(task: Task<Message>, out: mpsc::UnboundedSender<Message>) {
    if let Some(mut stream) = iced_runtime::task::into_stream(task) {
        tokio::spawn(async move {
            while let Some(action) = stream.next().await {
                if let Action::Output(message) = action {
                    let _ = out.send(message);
                }
            }
        });
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_click_on_the_panel_drives_the_browser_and_is_measured() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = HubConfig::discover(dir.path());
    let Some(browser) = &mut config.browser else {
        eprintln!("SKIPPED: no Chrome or Chromium installed");
        return;
    };
    browser.width = 400;
    browser.height = 300;
    let hub = SurfaceHub::new(config);
    let page = "data:text/html,<body style='margin:0;height:100vh;background:rgb(255,0,0)' onmousedown=\"document.body.style.background='rgb(0,0,255)'\"></body>";
    let opened = hub.open_browser(page).await.unwrap();
    let uri = opened.info.uri.clone();

    let status = BrowserStatus::Ready { executable: hub.browser_executable().unwrap().to_owned(), sandboxed: hub.browser_sandboxed() };
    let mut panel = Panel::new(PanelOptions::default(), status, dir.path());
    let (out, mut messages) = mpsc::unbounded_channel();
    spawn(panel.attach(opened.surface.clone(), "browser"), out.clone());
    let pump = |panel: &mut Panel, message: Message, out: &mpsc::UnboundedSender<Message>| {
        let (task, _) = panel.update(message);
        spawn(task, out.clone());
    };

    // The panel is as large as the surface, so panel and surface pixels agree.
    let size = Size::new(400.0, 300.0);
    pump(&mut panel, Message::Surface(uri.clone(), SurfaceMessage::Panel(size)), &out);
    let blue = |panel: &Panel| {
        let fb = panel.surface(&uri).and_then(|s| s.framebuffer())?;
        let i = ((fb.height() / 2) as usize * fb.width() as usize + (fb.width() / 2) as usize) * 4;
        let px = &fb.pixels()[i..i + 3];
        Some(px[2] > 200 && px[0] < 60)
    };
    let mut clicked = false;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let message = messages.recv().await.unwrap();
            pump(&mut panel, message, &out);
            if !clicked && panel.surface(&uri).and_then(|s| s.shown_seq()).is_some() {
                clicked = true;
                let press = PointerInput::Button { at: Point::new(200.0, 150.0), button: MouseButton::Left, pressed: true };
                pump(&mut panel, Message::Surface(uri.clone(), SurfaceMessage::Pointer { panel: size, input: press }), &out);
            }
            if blue(&panel) == Some(true) && panel.surface(&uri).and_then(|s| s.latency()).is_some() {
                return;
            }
        }
    })
    .await
    .expect("the click turned the page blue and the meter has a reading");
    let reading = panel.surface(&uri).unwrap().latency().unwrap();
    eprintln!("input to pixels: median {} us, p95 {} us", reading.median_us, reading.p95_us);
    hub.shutdown().await;
}
