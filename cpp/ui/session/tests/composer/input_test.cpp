// The editing machinery: undo history, popup triggers, mentions and the queue.

#include "support.hpp"

using namespace composer_test;

TEST_CASE("the undo history is bounded and redo clears on a new group") {
    UndoHistory h(2);
    const auto snap = [](std::string t) { return Snapshot{std::move(t), {}}; };
    h.begin(EditKind::other(), snap("a"));
    h.begin(EditKind::other(), snap("b"));
    h.begin(EditKind::other(), snap("c"));
    CHECK(h.undo(snap("d")) == snap("c"));
    CHECK(h.undo(snap("c")) == snap("b"));
    CHECK_MESSAGE(h.undo(snap("b")) == std::nullopt, "the oldest group fell off");
    CHECK(h.redo(snap("b")) == snap("c"));
    h.begin(EditKind::insert(false), snap("x"));
    CHECK(h.redo(snap("y")) == std::nullopt);
    CHECK(h.continues(EditKind::insert(false)) == false);
    h.begin(EditKind::insert(false), snap("x"));
    CHECK(h.continues(EditKind::insert(true)));
    h.extend(EditKind::insert(true));
    CHECK_FALSE(h.continues(EditKind::insert(false)));
    CHECK(h.continues(EditKind::insert(true)));
    h.begin(EditKind::del(), snap("z"));
    CHECK(h.continues(EditKind::del()));
    CHECK_FALSE(h.continues(EditKind::insert(false)));
}

TEST_CASE("edit kinds group typing, pastes and deletes") {
    CHECK(edit_kind(edit::Insert{'a'}) == EditKind::insert(false));
    CHECK(edit_kind(edit::Insert{' '}) == EditKind::insert(true));
    CHECK(edit_kind(edit::Paste{"\u00e9"}) == EditKind::insert(false));
    CHECK(edit_kind(edit::Paste{"ab"}) == EditKind::other());
    CHECK(edit_kind(edit::Paste{"\n"}) == EditKind::other());
    CHECK(edit_kind(edit::Backspace{}) == EditKind::del());
    CHECK(edit_kind(edit::Enter{}) == EditKind::other());
}

TEST_CASE("triggers open on a leading slash or a word-starting at") {
    CHECK(detect(0, "/re", 3) == Trigger{PopupKind::Command, 0, 0, 3, "re"});
    CHECK_MESSAGE(detect(1, "/re", 3) == std::nullopt, "a command starts the message");
    CHECK(detect(0, "x /re", 5) == std::nullopt);
    CHECK(detect(0, "see @ma rest", 6) == Trigger{PopupKind::File, 0, 4, 7, "m"});
    CHECK_MESSAGE(detect(0, "see @", 4) == std::nullopt, "the cursor before the @ is not inside the word");
    CHECK(detect(0, "me@x", 4) == std::nullopt);
}

TEST_CASE("popups wrap their highlight") {
    Popup p{Trigger{}, {1, 2, 3}, 0};
    p.step(-1);
    CHECK(p.selected == 2);
    p.step(2);
    CHECK(p.selected == 1);
    Popup empty;
    empty.step(1);
    CHECK(empty.selected == 0);
}

TEST_CASE("mentions are found at word starts, quoted or trimmed") {
    CHECK(token("a.rs") == "@a.rs");
    CHECK(token("a b.png") == "@\"a b.png\"");
    const std::vector<std::string_view> expected{"a.rs,", "a.rs", "b c", "d"};
    CHECK(mentioned("see @a.rs, @\"b c\" x@y @d") == expected);
    const auto files = resolve("@a.png @a.png @nope", [](std::string_view p) { return p == "a.png"; });
    REQUIRE(files.size() == 1);
    CHECK(files[0].path == "a.png");
    CHECK(files[0].mime == "image/png");
}

TEST_CASE("the queue edits by id and skips blank messages") {
    Queue q;
    q.push("one");
    q.push("two");
    const auto id = q.items.front().id;
    CHECK(q.edit(id, std::string("  ")));
    CHECK_FALSE(q.edit(99, std::nullopt));
    CHECK(q.pop_sendable() == QueuedMessage{1, "two"});
    CHECK(q.pop_sendable() == std::nullopt);
}

TEST_CASE("the content edits lines and selections") {
    auto c = Content::with_text("ab\ncd");
    CHECK(c.line_count() == 2);
    c.move_to({{0, 1}, Position{1, 1}});
    CHECK(c.selection() == "b\nc");
    c.perform(action::Edit{edit::Paste{"X\nY"}});
    CHECK(c.text() == "aX\nYd");
    CHECK(c.cursor() == Cursor{{1, 1}, std::nullopt});
    c.perform(action::Edit{edit::Backspace{}});
    c.perform(action::Edit{edit::Backspace{}});
    CHECK(c.text() == "aXd");
    c.perform(action::Move{Motion::DocumentStart});
    c.perform(action::Select{Motion::WordRight});
    CHECK(c.selection() == "aXd");
    c.perform(action::Move{Motion::Right});
    CHECK(c.cursor().position == Position{0, 3});
    c.perform(action::Edit{edit::Insert{U'\u00e9'}});
    c.perform(action::Move{Motion::Left});
    CHECK(c.cursor().position == Position{0, 3});
    c.perform(action::Edit{edit::Delete{}});
    CHECK(c.text() == "aXd");
    c.perform(action::SelectAll{});
    c.perform(action::Edit{edit::Delete{}});
    CHECK(c.is_empty());
}
