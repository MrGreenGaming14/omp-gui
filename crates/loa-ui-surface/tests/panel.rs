//! The Surfaces tab driven through `Panel::update`, with fake surfaces, plus its launcher view.

use loa_contracts::surface::{Surface, SurfaceInfo, SurfaceMode, SurfaceUri};
use loa_surfaces::testkit::FakeSurface;
use loa_ui_surface::panel::{Event, Message};
use loa_ui_surface::{Available, BrowserStatus, Options, Panel, picker};
use std::sync::Arc;

fn panel(dir: &std::path::Path) -> Panel {
    Panel::new(Options::default(), BrowserStatus::Ready { executable: "/usr/bin/chromium".into(), sandboxed: true }, dir)
}

fn fake(server: &str, name: &str, mode: SurfaceMode) -> Arc<FakeSurface> {
    let info = SurfaceInfo { uri: SurfaceUri::new(server, name).unwrap(), name: name.to_owned(), width: 8, height: 4, mode, mime: None };
    FakeSurface::new(info, None).0
}

#[tokio::test]
async fn the_launcher_asks_for_browser_document_vm_and_listed_surfaces() {
    let dir = tempfile::tempdir().unwrap();
    let mut panel = panel(dir.path());
    let deck = dir.path().join("deck.pptx");
    let declared = fake("figma", "board", SurfaceMode::Pixel);
    panel.set_available(vec![Available { info: declared.info(), server: "figma-mcp".into() }]);
    let vm = Event::OpenVm { address: "10.0.0.5:5901".into(), password: Some("secret".into()) };
    // (message, event): fields update silently, submits ask the app.
    let steps = [
        (Message::OpenUrl, None),
        (Message::Url("example.com".into()), None),
        (Message::OpenUrl, Some(Event::OpenBrowser("example.com".into()))),
        (Message::Picker(picker::Message::Choose(deck.clone())), Some(Event::OpenDocument(deck))),
        (Message::Address("10.0.0.5:5901".into()), None),
        (Message::Password("secret".into()), None),
        (Message::Connect, Some(vm)),
        (Message::OpenAvailable(declared.info.uri.clone()), Some(Event::Open(declared.info.uri.clone()))),
    ];
    for (message, event) in steps {
        assert_eq!(panel.update(message.clone()).1, event, "{message:?}");
    }
    assert_eq!(panel.pending(), Some(format!("Opening {}", declared.info.uri).as_str()));
    panel.failed("no Chrome or Chromium is installed");
    assert_eq!((panel.pending(), panel.error()), (None, Some("no Chrome or Chromium is installed")));
    panel.set_browser(BrowserStatus::Missing);
    assert_eq!(panel.update(Message::OpenUrl).1, None, "no browser, no request");
}

#[tokio::test]
async fn attached_surfaces_become_tabs_and_close() {
    let dir = tempfile::tempdir().unwrap();
    let mut panel = panel(dir.path());
    let (tab, doc) = (fake("browser", "tab1", SurfaceMode::Pixel), fake("docs", "1", SurfaceMode::Document));
    let _streams = (panel.attach(tab.clone(), "browser"), panel.attach(doc.clone(), "docs"));
    assert_eq!((panel.active(), panel.open_surfaces().count()), (Some(&doc.info.uri), 2));
    assert!(panel.surface(&tab.info.uri).unwrap().pixel().is_some() && panel.surface(&doc.info.uri).unwrap().document().is_some());
    let _ = panel.update(Message::Select(tab.info.uri.clone()));
    assert_eq!(panel.active(), Some(&tab.info.uri));
    assert_eq!(panel.update(Message::Close(tab.info.uri.clone())).1, Some(Event::Close(tab.info.uri.clone())));
    assert_eq!((panel.active(), panel.open_surfaces().count()), (Some(&doc.info.uri), 1), "the neighbour tab becomes active");
    let _ = panel.update(Message::Launcher);
    assert_eq!(panel.active(), None);
    panel.detach(&doc.info.uri);
    assert_eq!(panel.open_surfaces().count(), 0);
}

#[tokio::test]
async fn the_launcher_view_opens_a_typed_url() {
    let dir = tempfile::tempdir().unwrap();
    let mut panel = panel(dir.path());
    let messages: Vec<Message> = {
        let mut ui = iced_test::Simulator::with_size(iced::Settings::default(), iced::Size::new(420.0, 900.0), panel.view());
        ui.click("Search or enter a URL").unwrap();
        ui.typewrite("example.com");
        ui.tap_key(iced::keyboard::key::Named::Enter);
        ui.into_messages().collect()
    };
    let events: Vec<Event> = messages.into_iter().filter_map(|m| panel.update(m).1).collect();
    assert_eq!(events, [Event::OpenBrowser("example.com".into())]);
}
