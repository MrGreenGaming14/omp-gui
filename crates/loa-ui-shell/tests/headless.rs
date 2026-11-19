//! Headless runs of the whole frame through `iced_test`: clicks, typing,
//! panel toggling and the pixels of both themes.

use iced::widget::text;
use iced::{Color, Element, Settings, Size, Theme};
use iced_test::{Simulator, selector};
use loa_ui_shell::layout::{PANEL_ID, SIDEBAR_WIDTH};
use loa_ui_shell::{Event, Message, NodeStatus, PanelPrefs, PanelTab, Preference, SessionSummary, State, sidebar, topbar};

const WINDOW: Size = Size::new(1280.0, 800.0);

fn session(id: &str, title: &str, node: &str, updated: i64, waiting: bool) -> SessionSummary {
    SessionSummary {
        id: id.into(),
        title: title.into(),
        project: Some("harness".into()),
        node: node.into(),
        pinned: false,
        waiting,
        updated,
    }
}

fn shell(panel_open: bool) -> State {
    let (mut state, _) = State::new(Preference::System, PanelPrefs { open: panel_open, ..PanelPrefs::default() });
    state.set_nodes(vec![
        NodeStatus { key: "k-desk".into(), name: "desk".into(), online: true, local: true },
        NodeStatus { key: "k-laptop".into(), name: "laptop".into(), online: false, local: false },
    ]);
    state.set_projects(["harness".to_owned()]);
    state.set_sessions([
        session("s1", "Fix deploy script", "k-desk", 30, false),
        session("s2", "Write release notes", "k-laptop", 20, true),
    ]);
    state.set_tabs(vec![PanelTab::new("diff", "Diff"), PanelTab::new("terminal", "Terminal")]);
    state
}

fn view(state: &State) -> Element<'_, Message> {
    state.view(text("Chat goes here"), |tab| text(format!("{tab} body")).into(), std::convert::identity)
}

fn simulator(state: &State) -> Simulator<'_, Message> {
    Simulator::with_size(Settings::default(), WINDOW, view(state))
}

/// Feeds every message the simulator produced back into the state.
fn deliver(state: &mut State, messages: impl IntoIterator<Item = Message>) -> Vec<Event> {
    messages.into_iter().filter_map(|message| state.update(message).1).collect()
}

#[test]
fn new_session_button_asks_for_a_new_session() {
    let mut state = shell(false);
    let mut ui = simulator(&state);
    ui.click("New session").expect("New session button");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(deliver(&mut state, messages), [Event::NewSession]);
}

#[test]
fn nav_items_open_peers_tools_and_projects() {
    let mut state = shell(false);
    let mut ui = simulator(&state);
    ui.click("Peers").expect("Peers");
    ui.click("Tools").expect("Tools");
    ui.click("harness").expect("project row");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(
        deliver(&mut state, messages),
        [Event::OpenPeers, Event::OpenTools, Event::OpenProject("harness".into())]
    );
}

#[test]
fn typing_in_search_filters_recents_live() {
    let mut state = shell(false);
    let mut ui = simulator(&state);
    ui.click(selector::id(sidebar::SEARCH_ID)).expect("search input");
    ui.typewrite("deploy");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert!(deliver(&mut state, messages).is_empty());
    assert_eq!(state.sidebar().query(), "deploy");

    let mut ui = simulator(&state);
    assert!(ui.find("Fix deploy script").is_ok());
    assert!(ui.find("Write release notes").is_err(), "non matching session still listed");
}

#[test]
fn recents_carry_node_tags_and_waiting_badges() {
    let mut state = shell(false);
    {
        let mut ui = simulator(&state);
        assert!(ui.find("laptop").is_ok(), "remote session lacks its node tag");
        assert!(ui.find("desk").is_ok(), "local session lacks its node tag");
        assert!(ui.find(selector::id(sidebar::waiting_badge_id("s2"))).is_ok());
        assert!(ui.find(selector::id(sidebar::waiting_badge_id("s1"))).is_err());
    }

    let mut ui = simulator(&state);
    ui.click("Write release notes").expect("session row");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(deliver(&mut state, messages), [Event::OpenSession("s2".into())]);

    let mut ui = simulator(&state);
    assert!(ui.find(selector::id(topbar::WAITING_BADGE_ID)).is_ok());
    assert!(ui.find(selector::id(topbar::NODE_INDICATOR_ID)).is_ok());
    assert!(ui.find("offline").is_ok(), "node indicator must say the laptop is offline");
}

#[test]
fn panel_toggle_hides_and_restores_the_panel_on_its_tab() {
    let mut state = shell(true);
    let mut ui = simulator(&state);
    ui.click("Terminal").expect("terminal tab");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(deliver(&mut state, messages), [Event::SelectTab("terminal".into())]);

    let mut ui = simulator(&state);
    assert!(ui.find("terminal body").is_ok());
    ui.click(topbar::HIDE_PANEL).expect("hide panel button");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(deliver(&mut state, messages), [Event::PanelToggled { open: false }]);

    let mut ui = simulator(&state);
    assert!(ui.find(selector::id(PANEL_ID)).is_err(), "panel still drawn after hiding");
    ui.click(topbar::SHOW_PANEL).expect("show panel button");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(deliver(&mut state, messages), [Event::PanelToggled { open: true }]);

    let mut ui = simulator(&state);
    assert!(ui.find(selector::id(PANEL_ID)).is_ok());
    assert!(ui.find("terminal body").is_ok(), "panel forgot its active tab");
}

/// Renders the frame in `theme` and returns `(width, rgba)` of the snapshot.
fn render(state: &State, theme: &Theme, name: &str) -> (u32, Vec<u8>) {
    let snapshot = simulator(state).snapshot(theme).expect("snapshot");
    let dir = tempfile::tempdir().expect("temp dir");
    assert!(snapshot.matches_image(dir.path().join(name)).expect("write snapshot"));

    let path = std::fs::read_dir(dir.path())
        .expect("list temp dir")
        .map(|entry| entry.expect("dir entry").path())
        .find(|path| path.extension().is_some_and(|ext| ext == "png"))
        .expect("snapshot png");
    let mut reader = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).expect("open png")))
        .read_info()
        .expect("png header");
    let mut rgba = vec![0; reader.output_buffer_size().expect("buffer size")];
    let info = reader.next_frame(&mut rgba).expect("png frame");
    rgba.truncate(info.buffer_size());
    (info.width, rgba)
}

/// Color at a logical point of a snapshot taken at scale factor 2.
fn pixel((width, rgba): &(u32, Vec<u8>), x: f32, y: f32) -> Color {
    let (px, py) = ((x * 2.0) as usize, (y * 2.0) as usize);
    let at = (py * *width as usize + px) * 4;
    Color::from_rgba8(rgba[at], rgba[at + 1], rgba[at + 2], f32::from(rgba[at + 3]) / 255.0)
}

fn assert_close(actual: Color, expected: Color, what: &str) {
    let diff = (actual.r - expected.r).abs().max((actual.g - expected.g).abs()).max((actual.b - expected.b).abs());
    assert!(diff <= 3.0 / 255.0, "{what}: got {actual:?}, expected {expected:?}");
}

#[test]
fn both_themes_paint_the_frame_with_their_palettes() {
    let state = shell(true);
    let mut centers = Vec::new();
    for (theme, name) in [(loa_ui_shell::theme::dark(), "dark"), (loa_ui_shell::theme::light(), "light")] {
        let shot = render(&state, &theme, name);
        let palette = theme.extended_palette();

        let center = pixel(&shot, SIDEBAR_WIDTH + 200.0, 600.0);
        assert_close(center, palette.background.base.color, &format!("{name} center"));

        let sidebar = pixel(&shot, SIDEBAR_WIDTH - 4.0, 400.0);
        assert_close(sidebar, palette.background.weak.color, &format!("{name} sidebar"));

        let panel = pixel(&shot, WINDOW.width - 20.0, 600.0);
        assert_close(panel, palette.background.base.color, &format!("{name} panel"));

        let luminance = 0.2126 * center.r + 0.7152 * center.g + 0.0722 * center.b;
        assert_eq!(luminance < 0.5, palette.is_dark, "{name} theme has the wrong brightness");
        centers.push(center);
    }
    assert_ne!(centers[0], centers[1]);
}
