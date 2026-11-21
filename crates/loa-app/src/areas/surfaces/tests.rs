use crate::testing::TestApp;

#[test]
fn the_launcher_opens_from_the_palette_and_hub_events_reach_the_area() {
    let mut test = TestApp::new();
    for id in ["surfaces.browser", "surfaces.document", "surfaces.vm"] {
        test.command(id);
        assert!(test.app.ctx.shell.tabs().iter().any(|tab| tab.id == super::TAB), "{id} opens the Surfaces tab");
    }
    test.pump_feeds();
    assert!(test.app.ctx.app_tools.iter().any(|tool| tool.name.contains("screenshot")), "the surface tools are listed");
}
