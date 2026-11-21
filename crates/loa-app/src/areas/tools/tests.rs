use crate::testing::TestApp;

#[test]
fn the_tools_panel_opens_from_the_palette_and_sessions_get_the_app_tools() {
    let mut test = TestApp::new();
    test.palette("MCP servers");
    assert!(test.app.ctx.shell.tabs().iter().any(|tab| tab.id == super::TAB));
    test.command("sidebar.tools");
    assert!(test.app.ctx.app_tools.iter().any(|tool| tool.name == loa_projects::PROJECT_SEARCH));
    test.session("hello");
    let config = test.harness.seen().configs.remove(0);
    assert!(config.host_tools.iter().any(|tool| tool.name == loa_projects::PROJECT_SEARCH), "app tools reach sessions as host tools");
    test.prompt(&format!("tool {} {{}}", loa_projects::PROJECT_SEARCH));
    test.until("the host tool answered", |app| app.ctx.runtime.sessions().active().is_some_and(|session| !session.is_busy()));
    assert!(!test.harness.seen().prompts.is_empty());
    test.command("tools.shell.nowhere");
    assert!(!test.notice().starts_with("Unknown command"));
}
