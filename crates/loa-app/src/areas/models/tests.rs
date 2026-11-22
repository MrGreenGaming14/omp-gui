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

#[test]
fn picking_a_role_row_switches_role_and_thinking() {
    use super::*;
    use loa_contracts::harness::{Capabilities, ModelSwitch};
    let capabilities = Capabilities::builder().model_switch(ModelSwitch::PerTurn).build();
    let harness = FakeHarness::new("omp").capabilities(capabilities).models(&[("provider/deep", &["high"])]).effort();
    let mut test = TestApp::with(harness);
    let dir = test.dir.path().join("omp");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.yml"), "modelRoles:\n  DEEP: provider/deep:high\n").unwrap();
    test.session("hello");
    let model = ModelRef::new("provider", "deep");
    let catalog = Catalog { models: vec![], roles: vec![RoleAssignment { role: "DEEP".into(), model }] };
    let (state, _) = picker::State::open(catalog_to_view(&catalog), view::picker::Target::Session, None);
    test.area_mut::<State>().picker = ("omp".into(), Load::Ready(Picker { state, ids: HashMap::new() }));
    test.send(crate::app::Message::area(Message::Picker(Msg::Local(picker::Local::Pick(0)))));
    test.until("role switches", |app| app.ctx.runtime.sessions().active().unwrap().dom().root().unwrap().str_attr(attr::MODEL_ROLE) == Some("DEEP"));
    test.prompt("hello again");
    test.until("turn ends", |app| !app.ctx.composer.is_busy());
    let seen = test.harness.seen();
    assert_eq!(seen.models, ["provider/deep"]);
    assert_eq!(seen.efforts, ["high"]);
    assert!(seen.prompts.iter().any(|prompt| prompt == "The active model role is now `DEEP`.\n\nhello again"));
}

#[test]
fn picking_a_role_row_without_a_session_configures_the_next_one() {
    use super::*;
    let mut test = TestApp::with(FakeHarness::new("omp").models(&[("provider/deep", &["high"])]).effort());
    let dir = test.dir.path().join("omp");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.yml"), "modelRoles:\n  DEEP: provider/deep:high\n").unwrap();
    // A RULES.md the workflows screen refuses doesn't stop picking a role: only config.yml is read.
    std::fs::write(dir.join("RULES.md"), "<!-- omp-gui:workflow BROKEN -->\n").unwrap();
    let catalog = Catalog { models: vec![], roles: vec![RoleAssignment { role: "DEEP".into(), model: ModelRef::new("provider", "deep") }] };
    let (state, _) = picker::State::open(catalog_to_view(&catalog), view::picker::Target::Session, None);
    test.area_mut::<State>().picker = ("omp".into(), Load::Ready(Picker { state, ids: HashMap::new() }));
    test.send(crate::app::Message::area(Message::Picker(Msg::Local(picker::Local::Pick(0)))));
    test.session("hello");
    let seen = test.harness.seen();
    let config = seen.configs.last().unwrap();
    assert_eq!(config.model.as_deref(), Some("provider/deep"));
    assert_eq!(config.model_role.as_deref(), Some("DEEP"));
    assert_eq!(config.effort.as_deref(), Some("high"));
    assert_eq!(config.instructions.as_deref(), Some("The active model role is `DEEP`. Follow the RULES.md section for that role."));
}
