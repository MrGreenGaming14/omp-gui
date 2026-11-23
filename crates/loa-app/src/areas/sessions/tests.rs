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

#[test]
fn a_compacted_session_takes_over_what_was_typed_in_its_source() {
    let mut test = TestApp::new();
    let paste = |test: &mut TestApp, path: &str| drop(test.app.ctx.composer.attach_image(loa_ui_session::composer::pasted(path.into(), None)));
    let source = test.session("hello");
    test.prompt("/fork");
    test.until("the fork is active", |app| app.ctx.runtime.sessions().active_id().is_some_and(|id| id.as_str() != source));
    let compacted = test.active().unwrap().to_owned();
    test.type_text("typed in the fork");
    paste(&mut test, "fork.png");
    test.send(Message::OpenRecent(source.clone()));
    test.type_text("keep me");
    paste(&mut test, "source.png");

    // Tests have no small model to summarize with: the fork stands in for the compacted session.
    test.area_mut::<super::State>().compacted.insert(compacted.clone(), source.clone());
    test.send(Message::OpenRecent(compacted));
    assert!(test.app.ctx.runtime.sessions().get(&source).is_none(), "the source closed");
    assert_eq!(test.app.ctx.composer.text(), "keep me [Image #2] \n\ntyped in the fork [Image #1] ", "both texts stay, their images numbered apart");
    test.send(Message::Composer(loa_ui_session::composer::Message::Submit));
    test.until("the merged prompt ran", |app| !app.ctx.composer.is_busy());
    assert_eq!(test.harness.seen().prompts.last().map(String::as_str), Some("keep me [Image #2] \n\ntyped in the fork [Image #1]"));
}
