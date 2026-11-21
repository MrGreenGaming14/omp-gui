use crate::testing::{FakeHarness, TestApp};

#[test]
fn harnesses_switch_from_the_palette_and_recheck_in_their_tab() {
    let mut test = TestApp::with(FakeHarness::new("fake"));
    assert!(test.palette_titles().contains(&"Switch harness: fake".to_owned()));
    test.command("harness.fake");
    assert_eq!(test.app.ctx.sessions.harness.as_deref(), Some("fake"));
    test.command("harness.recheck");
    assert!(test.app.ctx.shell.tabs().iter().any(|tab| tab.id == super::TAB), "Recheck opens the Harnesses tab");
    test.until("the probe answered", |app| app.ctx.services.harnesses.get("fake").is_some_and(|harness| harness.option.available));
}
