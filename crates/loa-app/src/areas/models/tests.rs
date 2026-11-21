use crate::testing::{FakeHarness, TestApp};

#[test]
fn the_composer_lists_the_harness_models_and_switches_model_and_effort() {
    let mut test = TestApp::with(FakeHarness::new("fake").models(&[("a/one", &["low", "high"]), ("b/two", &[])]).effort());
    test.session("hello");
    test.until("the models are listed", |app| app.ctx.composer.models().0.len() == 2);
    test.command("composer.model.1");
    test.until("the model switched", |app| app.ctx.runtime.sessions().active().unwrap().dom().root().unwrap().str_attr(loa_core::attr::MODEL) == Some("b/two"));
    test.prompt("/model gone/x");
    test.until("the failure is shown", |app| app.ctx.notice().is_some_and(|notice| notice.contains("gone/x")));
    test.command("models.settings");
    assert_eq!(test.app.ctx.screen(), Some("models"));
}
