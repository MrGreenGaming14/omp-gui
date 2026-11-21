use crate::testing::TestApp;

#[test]
fn sandbox_settings_open_in_the_panel_and_its_tools_reach_sessions() {
    let mut test = TestApp::new();
    test.command("sandbox.settings");
    assert!(test.app.ctx.shell.tabs().iter().any(|tab| tab.id == super::TAB));
    assert!(matches!(test.area::<super::State>().view, super::View::Settings));
    test.session("hello");
    test.pump_feeds();
    assert!(test.area::<super::State>().runs().entries().is_empty(), "no run yet");
}
