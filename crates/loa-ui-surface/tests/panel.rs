//! The Surfaces tab driven through `Panel::update` and the tasks it returns,
//! with fake surfaces, plus a headless pass over its view.

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use iced::Task;
use iced_runtime::Action;
use loa_contracts::surface::{
    Document, Encoding, Frame, InputEvent, Rect, Surface, SurfaceError, SurfaceInfo, SurfaceMode, SurfaceUri,
};
use loa_ui_surface::panel::{Event, Message};
use loa_ui_surface::{Available, BrowserStatus, DocsRenderers, Panel, PanelOptions};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

struct Fake {
    info: SurfaceInfo,
    frames: broadcast::Sender<Arc<Frame>>,
    document: Option<Document>,
}

#[async_trait]
impl Surface for Fake {
    fn info(&self) -> SurfaceInfo {
        self.info.clone()
    }

    fn frames(&self) -> loa_contracts::surface::Result<broadcast::Receiver<Arc<Frame>>> {
        Ok(self.frames.subscribe())
    }

    async fn request_keyframe(&self) -> loa_contracts::surface::Result<()> {
        Ok(())
    }

    async fn input(&self, _event: InputEvent) -> loa_contracts::surface::Result<()> {
        Ok(())
    }

    async fn document(&self) -> loa_contracts::surface::Result<Document> {
        self.document.clone().ok_or(SurfaceError::Closed)
    }

    async fn screenshot(&self) -> loa_contracts::surface::Result<Bytes> {
        Err(SurfaceError::Closed)
    }
}

fn fake(server: &str, name: &str, mode: SurfaceMode, document: Option<Document>) -> Arc<Fake> {
    Arc::new(Fake {
        info: SurfaceInfo {
            uri: SurfaceUri::new(server, name).unwrap(),
            name: name.to_owned(),
            width: 8,
            height: 4,
            mode,
            mime: document.as_ref().map(|d| d.mime.clone()),
        },
        frames: broadcast::channel(8).0,
        document,
    })
}

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

fn panel(dir: &std::path::Path) -> Panel {
    let options = PanelOptions { documents: Some(Arc::new(DocsRenderers::new(loa_surface_docs::Fonts::none()))), ..PanelOptions::default() };
    let browser = BrowserStatus::Ready { executable: "/usr/bin/chromium".into(), sandboxed: true };
    Panel::new(options, browser, dir)
}

fn event(panel: &mut Panel, message: Message) -> Option<Event> {
    panel.update(message).1
}

#[tokio::test]
async fn the_launcher_asks_for_browser_document_and_vm_surfaces() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("deck.pptx"), b"").unwrap();
    let mut panel = panel(dir.path());
    assert_eq!(panel.active(), None);

    assert_eq!(event(&mut panel, Message::OpenUrl), None, "an empty URL opens nothing");
    event(&mut panel, Message::Url("example.com".into()));
    assert_eq!(event(&mut panel, Message::OpenUrl), Some(Event::OpenBrowser("example.com".into())));
    assert_eq!(panel.pending(), Some("Opening example.com"));
    panel.failed("no Chrome or Chromium is installed");
    assert_eq!((panel.pending(), panel.error()), (None, Some("no Chrome or Chromium is installed")));

    let deck = dir.path().join("deck.pptx");
    let picked = event(&mut panel, Message::Picker(loa_ui_surface::picker::Message::Choose(deck.clone())));
    assert_eq!(picked, Some(Event::OpenDocument(deck)));

    event(&mut panel, Message::Address("10.0.0.5:5901".into()));
    event(&mut panel, Message::Password("secret".into()));
    assert_eq!(
        event(&mut panel, Message::Connect),
        Some(Event::OpenVm { address: "10.0.0.5:5901".into(), password: Some("secret".into()) })
    );

    let declared = fake("figma", "board", SurfaceMode::Pixel, None);
    panel.set_available(vec![Available { info: declared.info(), server: "figma-mcp".into() }]);
    assert_eq!(event(&mut panel, Message::OpenAvailable(declared.info.uri.clone())), Some(Event::Open(declared.info.uri.clone())));

    panel.set_browser(BrowserStatus::Missing);
    event(&mut panel, Message::Url("example.com".into()));
    assert_eq!(event(&mut panel, Message::OpenUrl), None, "no browser, no request");
}

#[tokio::test]
async fn attached_surfaces_stream_into_tabs_and_close() {
    let dir = tempfile::tempdir().unwrap();
    let mut panel = panel(dir.path());
    let (out, mut messages) = mpsc::unbounded_channel();

    let tab = fake("browser", "tab1", SurfaceMode::Pixel, None);
    spawn(panel.attach(tab.clone(), "browser"), out.clone());
    assert_eq!(panel.active(), Some(&tab.info.uri));
    let frame = Frame {
        seq: 1,
        captured_us: 0,
        input_seq: 0,
        width: 8,
        height: 4,
        region: Rect { x: 0, y: 0, width: 8, height: 4 },
        encoding: Encoding::Rgba,
        keyframe: true,
        data: Bytes::from(vec![9u8; 8 * 4 * 4]),
    };
    // The renderer subscribes as its task starts.
    tokio::task::yield_now().await;
    while tab.frames.receiver_count() == 0 {
        tokio::task::yield_now().await;
    }
    tab.frames.send(Arc::new(frame)).unwrap();
    // The renderer also asks for a keyframe; deliver until the frame shows.
    while panel.surface(&tab.info.uri).unwrap().shown_seq().is_none() {
        let message = messages.recv().await.unwrap();
        assert!(matches!(&message, Message::Surface(uri, _) if *uri == tab.info.uri));
        let _ = panel.update(message);
    }
    let shown = panel.surface(&tab.info.uri).unwrap();
    assert_eq!(shown.shown_seq(), Some(1));
    assert_eq!(shown.framebuffer().map(|fb| (fb.width(), fb.height())), Some((8, 4)));

    // A spreadsheet renders its pages through the docs renderers.
    let mut book = rust_xlsxwriter::Workbook::new();
    book.add_worksheet().write_string(0, 0, "cell").unwrap();
    let xlsx = Document {
        name: "book.xlsx".into(),
        mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
        data: Bytes::from(book.save_to_buffer().unwrap()),
    };
    let doc = fake("docs", "1", SurfaceMode::Document, Some(xlsx));
    spawn(panel.attach(doc.clone(), "docs"), out.clone());
    assert_eq!(panel.active(), Some(&doc.info.uri));
    while panel.surface(&doc.info.uri).unwrap().page_count().is_none() {
        let _ = panel.update(messages.recv().await.unwrap());
    }
    assert_eq!(panel.surface(&doc.info.uri).unwrap().page_count(), Some(1));
    assert_eq!(panel.surface(&doc.info.uri).unwrap().notice(), None);

    assert_eq!(panel.open_surfaces().count(), 2);
    event(&mut panel, Message::Select(tab.info.uri.clone()));
    assert_eq!(panel.active(), Some(&tab.info.uri));
    assert_eq!(event(&mut panel, Message::Close(tab.info.uri.clone())), Some(Event::Close(tab.info.uri.clone())));
    assert_eq!(panel.open_surfaces().count(), 1);
    assert_eq!(panel.active(), Some(&doc.info.uri), "the neighbour tab becomes active");
    event(&mut panel, Message::Launcher);
    assert_eq!(panel.active(), None);
    panel.detach(&doc.info.uri);
    assert_eq!(panel.open_surfaces().count(), 0);
}

#[tokio::test]
async fn the_launcher_view_opens_a_typed_url() {
    use iced_test::Simulator;
    let dir = tempfile::tempdir().unwrap();
    let mut panel = panel(dir.path());
    let messages: Vec<Message> = {
        let mut ui = Simulator::with_size(iced::Settings::default(), iced::Size::new(420.0, 900.0), panel.view());
        ui.click("Search or enter a URL").unwrap();
        ui.typewrite("example.com");
        ui.tap_key(iced::keyboard::key::Named::Enter);
        ui.into_messages().collect()
    };
    let mut events = Vec::new();
    for message in messages {
        events.extend(event(&mut panel, message));
    }
    assert_eq!(events, [Event::OpenBrowser("example.com".into())]);
}
