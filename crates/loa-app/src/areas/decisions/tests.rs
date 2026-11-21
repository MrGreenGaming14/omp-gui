use crate::testing::TestApp;

#[test]
fn unattended_prompts_toggle_from_the_palette_and_are_saved() {
    let mut test = TestApp::new();
    let on = "Unattended prompts: deny approvals and reject plans after 5 min";
    assert!(test.palette_titles().contains(&on.to_owned()));
    test.command("decisions.unattended.on");
    assert!(test.palette_titles().contains(&"Unattended prompts: wait for someone".to_owned()));
    assert!(test.app.ctx.services.storage.config.load().unwrap().unattended != loa_settings::app::Config::default().unattended, "saved");
    test.command("decisions.settings");
    assert_eq!(test.app.ctx.screen(), Some("decisions"));
    test.click("Done");
    assert_eq!(test.app.ctx.screen(), None);

    test.session("hello");
    test.prompt("ask question");
    test.until("the card waits", |app| app.areas.get::<super::State>().focused().is_some());
}
