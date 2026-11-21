use crate::testing::TestApp;

#[test]
fn the_updates_screen_opens_from_the_palette() {
    let mut test = TestApp::new();
    assert!(test.palette_titles().iter().any(|title| title == "Check for updates"));
    test.command("updater.open");
    assert_eq!(test.app.ctx.screen(), Some("updater"));
}
