use crate::app::Message;
use crate::testing::TestApp;
use loa_view::session::transcript;

#[test]
fn slash_commands_branch_switch_mode_export_and_delete_sessions() {
    let mut test = TestApp::new();
    let first = test.session("hello");
    test.prompt("/fork");
    test.until("the fork is active", |app| app.ctx.runtime.sessions().active_id().is_some_and(|id| id.as_str() != first));
    let fork = test.active().unwrap().to_owned();
    let root = test.app.ctx.runtime.sessions().active().unwrap().dom().root().unwrap().str_attr(loa_core::attr::PARENT_SESSION).map(str::to_owned);
    assert_eq!(root.as_deref(), Some(first.as_str()), "the fork links its parent");

    test.send(Message::OpenRecent(first.clone()));
    let turn = loa_runtime::session::dom_turns(test.app.ctx.runtime.sessions().active().unwrap().dom()).last().expect("a turn").id;
    test.send(Message::area(super::Message::Chat(transcript::Intent::Rewind(turn))));
    test.until("the rewind is active", |app| app.ctx.runtime.sessions().active_id().is_some_and(|id| id.as_str() != fork && id.as_str() != first));
    let title = test.app.ctx.runtime.sessions().active().unwrap().summary().map(|summary| summary.title.clone());
    assert!(title.is_some_and(|title| title.ends_with("(rewound)")), "the rewound branch is named after its source");

    test.prompt("/compact");
    assert!(test.notice().starts_with("Compaction needs"), "no small model in tests: {}", test.notice());
    test.prompt("/plan");
    assert!(test.notice().contains("plan mode"), "a harness without read-only tools refuses: {}", test.notice());
    test.prompt("/model");
    assert!(test.notice().starts_with("Name the model"), "{}", test.notice());
    test.prompt("/export-xml");
    assert!(test.notice().starts_with("Exported to"), "{}", test.notice());

    test.send(Message::area(super::Message::Delete(fork.clone())));
    assert!(test.app.ctx.shell.sidebar().session(&fork).is_none(), "deleted from Recents");
    assert!(test.app.ctx.runtime.library().store().list().unwrap().iter().all(|listing| !format!("{listing:?}").contains(&fork)));
}
