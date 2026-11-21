use crate::app::Message;
use crate::testing::TestApp;

#[test]
fn slash_commands_fork_switch_mode_and_delete_sessions() {
    let mut test = TestApp::new();
    let first = test.session("hello");
    test.prompt("/fork");
    test.until("the fork is active", |app| app.ctx.sessions.active_id().is_some_and(|id| id.as_str() != first));
    let fork = test.active().unwrap().to_owned();
    let root = test.app.ctx.sessions.active().unwrap().dom().root().unwrap().str_attr(loa_core::attr::PARENT_SESSION).map(str::to_owned);
    assert_eq!(root.as_deref(), Some(first.as_str()), "the fork links its parent");

    test.prompt("/plan");
    assert!(test.notice().contains("plan mode"), "a harness without read-only tools refuses: {}", test.notice());
    test.prompt("/model");
    assert!(test.notice().starts_with("Name the model"), "{}", test.notice());

    test.send(Message::area(super::Message::Delete(fork.clone())));
    assert!(test.app.ctx.shell.sidebar().session(&fork).is_none(), "deleted from Recents");
    assert!(test.app.ctx.services.library.store().list().unwrap().len() <= 1);
}
