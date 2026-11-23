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

#[test]
fn signing_out_removes_the_picked_sign_in_and_reads_the_rows_again() {
    use super::*;
    use loa_settings::models::rpc::{LoginProvider, LogoutAccount};
    let mut test = TestApp::new();
    let mut omp = loa_models::testing::FakeOmp::default();
    omp.providers = vec![LoginProvider { id: "anthropic".into(), name: "Anthropic".into(), available: true, authenticated: true }];
    let account = LogoutAccount { credential_id: 7, provider: "anthropic".into(), label: "me@example.com".into(), detail: "Claude Max".into(), kind: "oauth".into(), active: true };
    omp.credentials = vec![account].into();
    omp.remaining_source = Some("ANTHROPIC_API_KEY".into());
    test.omp(omp);
    test.command("models.sign-in");
    let auth = |test: &TestApp| match &test.area::<State>().signin {
        Load::Ready(signin) => signin.providers()[0].auth.clone(),
        _ => panic!("the sign in rows were not read"),
    };
    assert_eq!(auth(&test), view::Auth::Login { signed_in: true, account: None });
    test.click("Sign out");
    // One stored sign in, one Remove: clicking it removes credential 7 in omp.
    test.click("Remove");
    assert_eq!(auth(&test), view::Auth::Login { signed_in: false, account: None }, "the rows are read again");
    assert!(loa_ui_kit::testkit::click_label(test.app.view(), "Sign out").is_empty(), "nothing is left to sign out of");
    // The outcome stays on the row through the refresh until dismissed.
    test.click("Dismiss");
    assert!(!loa_ui_kit::testkit::click_label(test.app.view(), "Sign in").is_empty(), "the row signs in again");
}
