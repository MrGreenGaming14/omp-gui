use std::sync::Arc;

use crate::app::Message;
use crate::testing::{FakeHarness, TestApp};

#[test]
fn harnesses_switch_from_the_palette_and_recheck_in_their_tab() {
    let mut test = TestApp::with(FakeHarness::new("fake"));
    assert!(test.palette_titles().contains(&"Switch harness: fake".to_owned()));
    test.command("harness.fake");
    assert_eq!(test.app.ctx.runtime.sessions().harness.as_deref(), Some("fake"));
    test.command("harness.recheck");
    assert!(test.app.ctx.shell.tabs().iter().any(|tab| tab.id == super::TAB), "Recheck opens the Harnesses tab");
    test.until("the probe answered", |app| app.ctx.runtime.harnesses().get("fake").is_some_and(|harness| harness.available));
}

#[test]
fn switching_the_harness_of_an_empty_session_keeps_what_was_typed_in_it() {
    let mut test = TestApp::new();
    crate::services::harness::add(test.app.ctx.runtime.harnesses_mut(), Arc::new(FakeHarness::new("other")), "Other", true, None, None);
    test.type_text("center");
    test.command("session.new");
    test.until("the new session is active", |app| app.ctx.runtime.sessions().active_id().is_some());
    let empty = test.active().unwrap().to_owned();
    test.type_text("for the other harness");

    test.command("harness.other");
    test.until("a session runs on the other harness", |app| app.ctx.runtime.sessions().active().and_then(|session| session.harness_id()) == Some("other"));
    assert!(test.app.ctx.runtime.sessions().get(&empty).is_none(), "the empty session was discarded");
    assert_eq!(test.app.ctx.composer.text(), "for the other harness", "its replacement took over its text");
    let replacement = test.active().unwrap().to_owned();
    test.send(Message::area(crate::areas::sessions::Message::Delete(replacement)));
    assert_eq!(test.app.ctx.composer.text(), "center", "the center kept its own");
}

#[test]
fn a_harness_switch_that_fails_to_start_gives_the_typed_text_back() {
    let mut test = TestApp::new();
    crate::services::harness::add(test.app.ctx.runtime.harnesses_mut(), Arc::new(FakeHarness::new("other").no_start()), "Other", true, None, None);
    test.type_text("center");
    test.command("session.new");
    test.until("the new session is active", |app| app.ctx.runtime.sessions().active_id().is_some());
    test.type_text("for the other harness");

    test.command("harness.other");
    test.until("the start failed", |app| app.ctx.runtime.sessions().active_id().is_none() && app.ctx.runtime.sessions().starting().is_none());
    assert_eq!(test.app.ctx.composer.text(), "for the other harness\n\ncenter", "the text is back, ahead of the center's own");

    test.command("harness.fake");
    test.until("a session runs on the fake harness", |app| app.ctx.runtime.sessions().active().is_some());
    assert_eq!(test.app.ctx.composer.text(), "", "the next session doesn't get the text again");
}
