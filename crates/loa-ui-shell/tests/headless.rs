//! Headless runs of the whole frame through `iced_test`: clicks, typing,
//! panel toggling and the pixels of both themes.

use iced::widget::text;
use iced::{Color, Element, Settings, Size, Theme};
use iced_test::{Simulator, selector};
use loa_ui_kit::theme::{ColorRole, Variant, for_theme};
use loa_ui_shell::layout::{PANEL_ID, SIDEBAR_WIDTH, project_tab_close_id, project_tab_id};
use loa_ui_shell::{Event, Message, NodeFilter, NodeStatus, PanelPrefs, PanelTab, SessionSummary, State, sidebar, topbar};

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
    let (mut state, _) = State::new(Default::default(), PanelPrefs { open: panel_open, ..PanelPrefs::default() });
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
    state.view(text("Chat goes here"), |tab| text(format!("{tab} body")).into(), None, std::convert::identity)
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
fn footer_items_open_usage_configs_settings_and_projects() {
    let mut state = shell(false);
    let mut ui = simulator(&state);
    ui.click("Usage").expect("Usage");
    ui.click("Configs").expect("Configs");
    ui.click("Settings").expect("Settings");
    ui.click("harness").expect("project row");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(
        deliver(&mut state, messages),
        [Event::OpenUsage, Event::OpenConfigs, Event::OpenSettings, Event::OpenProject("harness".into())]
    );
}

fn with_project_files(state: &State) -> Element<'_, Message> {
    state.view(
        text("Chat goes here"),
        |tab| text(format!("{tab} body")).into(),
        Some(text("main.rs marker").into()),
        std::convert::identity,
    )
}

#[test]
fn project_files_draw_under_the_active_project_only() {
    let mut state = shell(false);
    state.set_projects(["harness".to_owned(), "other".to_owned()]);

    state.set_active_project(Some("harness".into()));
    let mut ui = Simulator::with_size(Settings::default(), WINDOW, with_project_files(&state));
    assert!(ui.find("main.rs marker").is_ok(), "the active project lacks its file list");
    assert!(ui.find("other").is_ok());
    drop(ui);

    state.set_active_project(Some("elsewhere".into()));
    let mut ui = Simulator::with_size(Settings::default(), WINDOW, with_project_files(&state));
    assert!(ui.find("main.rs marker").is_err(), "a file list was drawn without an active project row");
    drop(ui);

    state.set_active_project(None);
    let mut ui = Simulator::with_size(Settings::default(), WINDOW, with_project_files(&state));
    assert!(ui.find("main.rs marker").is_err());
}

#[test]
fn pinning_a_project_moves_it_first_emits_the_save_event_and_search_still_filters() {
    let mut state = shell(false);
    state.set_projects(["alpha".to_owned(), "beta".to_owned(), "gamma".to_owned()]);
    assert_eq!(state.sidebar().projects(), ["alpha", "beta", "gamma"]);
    let events = deliver(&mut state, [Message::Sidebar(sidebar::Message::ToggleProjectPin("gamma".into()))]);
    assert_eq!(events, [Event::SetProjectPinned { project: "gamma".into(), pinned: true }]);
    assert_eq!(state.sidebar().projects(), ["gamma", "alpha", "beta"]);

    // The pin is a hover icon, as on session rows; the row itself still
    // draws, now first.
    let mut ui = simulator(&state);
    assert!(ui.find("gamma").is_ok(), "the pinned project is listed");
    drop(ui);

    let _ = deliver(&mut state, [Message::Sidebar(sidebar::Message::Search("gam".into()))]);
    assert_eq!(state.sidebar().projects(), ["gamma"]);
    let _ = deliver(&mut state, [Message::Sidebar(sidebar::Message::Search("alp".into()))]);
    assert_eq!(state.sidebar().projects(), ["alpha"]);
    let _ = deliver(&mut state, [Message::Sidebar(sidebar::Message::Search(String::new()))]);

    let events = deliver(&mut state, [Message::Sidebar(sidebar::Message::ToggleProjectPin("gamma".into()))]);
    assert_eq!(events, [Event::SetProjectPinned { project: "gamma".into(), pinned: false }]);
    assert_eq!(state.sidebar().projects(), ["alpha", "beta", "gamma"]);

    // A pin on a project that is no longer listed stays saved but hidden.
    state.set_pinned_projects(["gone".to_owned()]);
    assert_eq!(state.sidebar().projects(), ["alpha", "beta", "gamma"]);
    assert_eq!(state.sidebar().pinned_projects().len(), 1);
}

#[test]
fn machine_switcher_lists_peers_and_filters_recents() {
    let mut state = shell(false);
    {
        let mut ui = simulator(&state);
        assert!(ui.find("Fix deploy script").is_ok(), "this computer's session is listed");
        assert!(ui.find("Write release notes").is_err(), "the laptop's session is not");
        ui.click(selector::id(sidebar::MACHINE_SWITCHER_ID)).expect("machine switcher");
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(deliver(&mut state, messages).is_empty());
    }
    let mut ui = simulator(&state);
    assert!(ui.find("1 need you").is_ok(), "the laptop's waiting session is counted");
    ui.click("laptop").expect("laptop row");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert!(deliver(&mut state, messages).is_empty());
    assert_eq!(state.sidebar().machine(), NodeFilter::Node("k-laptop".into()));

    let mut ui = simulator(&state);
    assert!(ui.find("Write release notes").is_ok());
    assert!(ui.find("Fix deploy script").is_err());
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
    let _ = state.update(Message::Sidebar(sidebar::Message::PickMachine(NodeFilter::All)));
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
    assert!(ui.find("harness on laptop").is_ok(), "the breadcrumb names the remote machine");
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
    ui.click(selector::id(topbar::PANEL_TOGGLE_ID)).expect("panel toggle");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(deliver(&mut state, messages), [Event::PanelToggled { open: false }]);

    let mut ui = simulator(&state);
    assert!(ui.find(selector::id(PANEL_ID)).is_err(), "panel still drawn after hiding");
    ui.click(selector::id(topbar::PANEL_TOGGLE_ID)).expect("panel toggle");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(deliver(&mut state, messages), [Event::PanelToggled { open: true }]);

    let mut ui = simulator(&state);
    assert!(ui.find(selector::id(PANEL_ID)).is_ok());
    assert!(ui.find("terminal body").is_ok(), "panel forgot its active tab");
}

fn with_project_tabs(enabled: bool) -> State {
    let mut state = shell(false);
    state.set_project_tabs(enabled);
    state.set_open_projects(vec!["harness".into(), "docs".into()]);
    state.set_active_project(Some("harness".into()));
    state
}

#[test]
fn project_tabs_open_their_project_and_close_without_opening() {
    let mut state = with_project_tabs(true);
    {
        let mut ui = simulator(&state);
        assert!(ui.find(selector::id(project_tab_id("harness"))).is_ok());
        assert!(ui.find(selector::id(project_tab_id("docs"))).is_ok());
    }

    let mut ui = simulator(&state);
    ui.click(selector::id(project_tab_id("docs"))).expect("docs tab");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(deliver(&mut state, messages), [Event::OpenProject("docs".into())]);

    let mut ui = simulator(&state);
    ui.click(selector::id(project_tab_close_id("docs"))).expect("docs close button");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(deliver(&mut state, messages), [Event::CloseProjectTab("docs".into())]);
}

#[test]
fn project_tabs_off_draws_no_strip() {
    let state = with_project_tabs(false);
    let mut ui = simulator(&state);
    assert!(ui.find(selector::id(project_tab_id("harness"))).is_err());
    assert!(ui.find(selector::id(project_tab_close_id("harness"))).is_err());
    assert!(ui.find(selector::id(project_tab_id("docs"))).is_err());
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

/// RGBA at a logical point of a snapshot taken at scale factor 2.
fn pixel((width, rgba): &(u32, Vec<u8>), x: f32, y: f32) -> [u8; 4] {
    let (px, py) = ((x * 2.0) as usize, (y * 2.0) as usize);
    let at = (py * *width as usize + px) * 4;
    [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
}

fn assert_close(actual: [u8; 4], expected: Color, what: &str) {
    let diff = actual.iter().zip(expected.into_rgba8()).take(3).map(|(a, e)| a.abs_diff(e)).max();
    assert!(diff <= Some(3), "{what}: got {actual:?}, expected {expected:?}");
}

#[test]
fn both_themes_paint_the_frame_with_their_palettes() {
    let state = shell(true);
    let mut centers = Vec::new();
    for (theme, name) in [(loa_ui_shell::theme::dark(), "dark"), (loa_ui_shell::theme::light(), "light")] {
        let shot = render(&state, &theme, name);
        let tokens = for_theme(&theme);

        let center = pixel(&shot, SIDEBAR_WIDTH + 200.0, 600.0);
        assert_close(center, tokens.color(ColorRole::Ground), &format!("{name} center"));

        let sidebar = pixel(&shot, SIDEBAR_WIDTH - 4.0, 400.0);
        assert_close(sidebar, tokens.color(ColorRole::Side), &format!("{name} sidebar"));

        let panel = pixel(&shot, WINDOW.width - 20.0, 600.0);
        assert_close(panel, tokens.color(ColorRole::Side), &format!("{name} panel"));

        let luminance = (0.2126 * f32::from(center[0]) + 0.7152 * f32::from(center[1]) + 0.0722 * f32::from(center[2])) / 255.0;
        assert_eq!(luminance < 0.5, tokens.variant == Variant::Dark, "{name} theme has the wrong brightness");
        centers.push(center);
    }
    assert_ne!(centers[0], centers[1]);
}
