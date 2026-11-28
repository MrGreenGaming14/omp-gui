// Port of the tests in `loa-ui-shell/src/palette/mod.rs` (the click simulation of the third
// becomes `Activate` on the rows the view would draw as buttons).

#include <doctest/doctest.h>

#include <string>
#include <vector>

#include "loa/ui/shell/palette.hpp"

using namespace loa::ui::shell;
using namespace loa::ui::shell::palette;

namespace {

State make(std::vector<Command> commands) {
    State state(Platform::Other);
    state.set_commands(std::move(commands));
    state.force_open();
    return state;
}

// Sends the messages and lists the rows, as `# title` at a section start.
std::vector<std::string> listed(State& state, std::vector<Message> messages) {
    for (auto& m : messages) (void)state.update(std::move(m));
    std::vector<std::string> out;
    for (const auto& r : state.rows()) out.push_back((r.starts_section ? "# " : "") + state.command_of(r).title);
    return out;
}

Message q(std::string s) { return msg::QueryChanged{std::move(s)}; }

} // namespace

TEST_CASE("filtering ranks, groups sections and prefers recent") {
    const char* commands[][3] = {{"find", "Find in files", "File"}, {"tree", "File tree", "View"},       {"open", "Open file", "File"},
                                 {"format", "Format imports", "File"}, {"panel", "Toggle panel", "View"}, {"term", "Toggle terminal", "View"}};
    std::vector<Command> list;
    for (const auto& c : commands) list.emplace_back(CommandId(c[0]), c[1], c[2]);
    auto state = make(std::move(list));
    using V = std::vector<std::string>;
    CHECK(listed(state, {q("fi")}) == V{"# Find in files", "Open file", "Format imports", "# File tree"});
    CHECK(listed(state, {q("tog")}) == V{"# Toggle panel", "Toggle terminal"});
    CHECK(listed(state, {q("terminal"), msg::Submit{}, msg::Open{}, q("tog")}) == V{"# Toggle terminal", "Toggle panel"});
    CHECK(listed(state, {q("pan")}) == V{"# Toggle panel"});
    CHECK(listed(state, {q("zzz")}) == V{});
    CHECK(listed(state, {msg::Close{}, msg::OpenShortcuts{}}) == V{});
    listed(state, {msg::Open{}, q("tp")});
    std::vector<std::size_t> matched;
    for (const auto& run : state.rows()[0].runs)
        if (run.matched) matched.push_back(run.begin);
    CHECK(matched == std::vector<std::size_t>{0, 7});
}

TEST_CASE("navigation skips disabled, wraps, scrolls and closes") {
    std::vector<Command> list;
    for (int i = 0; i < 20; ++i) {
        const auto n = std::to_string(i);
        list.push_back(Command(CommandId("c" + n), "Command " + std::string(i < 10 ? "0" : "") + n, "S").with_enabled(i != 1));
    }
    auto state = make(std::move(list));
    struct Step {
        int steps;
        Message message;
        std::size_t want;
        float offset;
    };
    for (const auto& [steps, message, want, offset] : {Step{10, msg::SelectNext{}, 11, 12.0F}, Step{11, msg::SelectPrevious{}, 19, 20.0F}}) {
        listed(state, std::vector<Message>(static_cast<std::size_t>(steps), message));
        REQUIRE(state.selected());
        CHECK(state.rows()[*state.selected()].command == want);
        CHECK(state.scroll_offset() == doctest::Approx(HEADER_HEIGHT + offset * ROW_HEIGHT - MAX_LIST_HEIGHT));
    }
    CHECK(state.update(msg::Activate{1}).second == std::nullopt); // disabled rows do not run
    CHECK(state.update(msg::Toggle{}).second == Event{event::Closed{}});
    CHECK(state.update(msg::Close{}).second == std::nullopt);
    CHECK(state.update(msg::Submit{}).second == std::nullopt);
}

TEST_CASE("shortcuts mode lists keyed commands and activating runs rows") {
    auto panel = Command(CommandId(ids::PANEL_TOGGLE), "Toggle panel", "View")
                     .with_shortcut(Keymap::defaults(Platform::Other).chord_for(CommandId(ids::PANEL_TOGGLE)));
    auto keys = Command(CommandId("keys"), "Sessions", "Keys").with_keys("1\u20139").with_enabled(false);
    auto state = make({Command(CommandId("a"), "Alpha", "S"), std::move(panel), std::move(keys)});
    listed(state, {msg::OpenShortcuts{}});
    std::vector<std::optional<std::string>> hints;
    for (const auto* c : state.entries()) hints.push_back(state.hint(*c));
    CHECK(hints == std::vector<std::optional<std::string>>{"Ctrl+B", "1\u20139"});
    // Clicking both rows: the disabled one is static text and runs nothing.
    std::vector<Event> events;
    for (std::size_t row : {std::size_t{0}, std::size_t{1}}) {
        if (auto e = state.update(msg::Activate{row}).second) events.push_back(*e);
    }
    CHECK(events == std::vector<Event>{event::Run{CommandId(ids::PANEL_TOGGLE)}});
    listed(state, {msg::Open{}});
    CHECK(!state.shortcuts_only());
    CHECK(state.entries().size() == 3);
}

TEST_CASE("open focuses the search and closed palettes ignore input") {
    State state(Platform::Other);
    state.set_commands({Command(CommandId("a"), "Alpha", "S")});
    CHECK(state.update(msg::SelectNext{}).first.empty());
    CHECK(state.update(msg::Close{}).second == std::nullopt);
    const auto [task, ev] = state.update(msg::Toggle{});
    CHECK(state.is_open());
    CHECK(ev == std::nullopt);
    CHECK(task.ops.front() == Op{op::Focus{std::string(INPUT_ID)}});
    CHECK(state.update(msg::Submit{}).second == Event{event::Run{CommandId("a")}});
    CHECK(!state.is_open());
}
