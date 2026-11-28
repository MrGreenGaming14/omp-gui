// Selection gestures and copies. Port of the `selection.rs` tests, driven through the gesture
// API instead of a simulated window.

#include <doctest/doctest.h>

#include "loa/ui/session/transcript/selection.hpp"

using namespace loa::ui::session::transcript;

TEST_CASE("a link click navigates but dragging from the link copies multiline text") {
    Selection sel;
    const std::string text = "\u00e9clair\n    second";
    const auto block = sel.add(text, {{0, std::string_view("\u00e9clair").size(), "https://example.com"}});
    // A click on the link opens it and selects nothing.
    sel.press({block, 0}, 2, 15);
    CHECK(sel.release() == "https://example.com");
    CHECK(sel.copy().empty());
    // A drag from the link to past the end selects everything.
    sel.press({block, 0}, 2, 15);
    sel.drag(Endpoint{block, text.size()}, 200, 45);
    CHECK_FALSE(sel.release());
    CHECK(sel.copy() == "\u00e9clair\n    second");
    // A click elsewhere clears it.
    sel.press({block, text.size()}, 300, 150);
    CHECK_FALSE(sel.release());
    CHECK(sel.copy().empty());
}

TEST_CASE("dragging across paragraphs copies both blocks in reading order") {
    Selection sel;
    const auto first = sel.add("first");
    const auto second = sel.add("second");
    sel.press({first, 0}, 2, 15);
    sel.drag(Endpoint{second, 6}, 200, 55);
    sel.release();
    CHECK(sel.copy() == "first\n\nsecond");
    // Dragging backwards selects the same text.
    sel.press({second, 6}, 200, 55);
    sel.drag(Endpoint{first, 0}, 2, 15);
    sel.release();
    CHECK(sel.copy() == "first\n\nsecond");
    CHECK((sel.range(first) == std::pair<std::size_t, std::size_t>{0, 5}));
}

TEST_CASE("a small move is a click, not a drag") {
    Selection sel;
    const auto b = sel.add("hello world");
    sel.press({b, 0}, 10, 10);
    sel.drag(Endpoint{b, 2}, 11, 11);
    sel.release();
    CHECK(sel.copy().empty());
}
