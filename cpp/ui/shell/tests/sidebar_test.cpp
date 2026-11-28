// Port of the tests in `loa-ui-shell/src/sidebar.rs`; the render checks become checks on the
// state the view draws from.

#include <doctest/doctest.h>

#include <string>
#include <vector>

#include "loa/ui/shell/sidebar.hpp"

using namespace loa::ui::shell;
using namespace loa::ui::shell::sidebar;
namespace is = loa::view::shell::sidebar_intent;

namespace {

SessionSummary summary(const std::string& id, const std::string& node, std::int64_t updated, bool pinned) {
    SessionSummary s;
    s.id = id;
    s.title = "Title " + id;
    if (id == "p") s.project = "Harness";
    s.node = node;
    s.pinned = pinned;
    s.updated = updated;
    return s;
}

std::vector<std::string> ids(const std::vector<const SessionSummary*>& list) {
    std::vector<std::string> out;
    for (const auto* s : list) out.push_back(s->id);
    return out;
}

NodeStatus node(std::string key, std::string name, bool local) { return NodeStatus{std::move(key), std::move(name), local, local}; }

std::optional<Event> send(State& state, Message m) { return state.update(std::move(m)).second; }

using V = std::vector<std::string>;
using P = std::vector<std::string_view>;

} // namespace

TEST_CASE("page sizes match the Rust sidebar") {
    CHECK(RECENTS_PAGE == 50);
    CHECK(PROJECT_SESSIONS == 8);
    CHECK(SEARCH_ID == "loa-sidebar-search");
}

TEST_CASE("machine filter, pins and search shape the lists") {
    State state;
    state.set_sessions({summary("a", "k-desk", 100, false), summary("b", "k-laptop", 300, false), summary("c", "k-desk", 200, false),
                        summary("x", "k-desk", 10, true), summary("p", "k-desk", 5, false)});
    state.set_pins({"later", "x"});
    state.set_nodes({node("k-desk", "desk", true), node("k-laptop", "laptop", false)});
    CHECK(ids(state.recents()) == V{"c", "a", "p"});
    CHECK(ids(state.pinned()) == V{"x"});
    send(state, msg::PickMachine{NodeFilter{}});
    CHECK(ids(state.recents()) == V{"b", "c", "a", "p"});
    CHECK(send(state, msg::TogglePin{"c"}) == Event{is::SetPinned{"c", true}});
    state.upsert_session(summary("c", "k-desk", 400, false));
    state.upsert_session(summary("later", "k-laptop", 3, false));
    CHECK(ids(state.pinned()) == V{"c", "x", "later"}); // pins survive fresh summaries
    state.set_projects({"Harness", "Website"});
    struct Case {
        const char* query;
        V recents;
        P projects;
    };
    for (const auto& c : {Case{"LAPTOP", {"b"}, {}}, Case{"harness title p", {"p"}, {}}, Case{"web", {}, {"Website"}}}) {
        CAPTURE(c.query);
        send(state, msg::Search{c.query});
        CHECK(ids(state.recents()) == c.recents);
        CHECK(state.projects() == c.projects);
    }
    CHECK(send(state, msg::OpenSession{"a"}) == Event{is::OpenSession{"a"}});
    CHECK(send(state, msg::ToggleProjectPin{"beta"}) == Event{is::SetProjectPinned{"beta", true}});
    CHECK(send(state, msg::ToggleProjectPin{"beta"}) == Event{is::SetProjectPinned{"beta", false}});
    CHECK(state.active() == std::optional<std::string>("a"));
}

TEST_CASE("recents show a page at a time and a new machine starts over") {
    State state;
    state.set_nodes({node("k-desk", "desk", true), node("k-laptop", "laptop", false)});
    std::vector<SessionSummary> many;
    for (int n = 0; n < 120; ++n) {
        auto s = summary("s" + std::to_string(n), "k-desk", n, false);
        s.title = "Session";
        many.push_back(std::move(s));
    }
    state.set_sessions(std::move(many));
    CHECK(state.recents().size() == 120); // every match stays listed for navigation
    CHECK(state.recents_limit() == RECENTS_PAGE);
    send(state, msg::ShowMoreRecents{});
    CHECK(state.recents_limit() == 2 * RECENTS_PAGE);
    auto renamed = summary("s0", "k-desk", 500, false);
    renamed.title = "Renamed";
    state.upsert_session(renamed);
    CHECK(state.recents_limit() == 2 * RECENTS_PAGE); // a new summary keeps the pages the user opened
    CHECK(ids(state.recents())[0] == "s0");           // the order follows the update
    send(state, msg::PickMachine{NodeFilter{}});
    CHECK(state.recents_limit() == RECENTS_PAGE);
}

TEST_CASE("a session moves to a project or out of every project") {
    State state;
    state.set_projects({"alpha", "beta"});
    state.set_sessions({summary("a", "k", 1, false)});
    send(state, msg::ToggleMoveMenu{"a"});
    CHECK(state.move_menu() == std::optional<std::string>("a"));
    CHECK(send(state, msg::MoveTo{"a", "beta"}) == Event{is::MoveSession{"a", "beta"}});
    CHECK(!state.move_menu()); // picking closes the list
    send(state, msg::ToggleMoveMenu{"a"});
    send(state, msg::ToggleMoveMenu{"a"});
    CHECK(!state.move_menu()); // the folder button closes it again
    CHECK(send(state, msg::MoveTo{"a", std::nullopt}) == Event{is::MoveSession{"a", std::nullopt}});
}

TEST_CASE("the active project lists its sessions under its row") {
    const auto harness = [](std::string id, std::int64_t updated) {
        auto s = summary(id, "k-desk", updated, false);
        s.project = "harness";
        s.title = "Harness task " + id;
        return s;
    };
    State state;
    state.set_nodes({node("k-desk", "desk", true)});
    state.set_projects({"harness", "other"});
    state.set_sessions({harness("s1", 30)});
    state.active_project = "other";
    CHECK(state.highlighted_project() == std::optional<std::string_view>("other"));
    CHECK(state.project_sessions("other").first.empty()); // "No sessions yet"
    std::vector<SessionSummary> more;
    for (int i = 0; i < 9; ++i) more.push_back(harness("h" + std::to_string(i), 100 + i));
    state.upsert_sessions(std::move(more));
    state.active_project = "harness";
    const auto [shown, hidden] = state.project_sessions("harness");
    CHECK(shown.size() == PROJECT_SESSIONS);
    CHECK(shown.size() + hidden == 10); // "All 10 sessions"
    CHECK(shown.front()->id == "h8");
    state.active_project.reset();
    CHECK(!state.highlighted_project());
}

TEST_CASE("running state survives summaries, yields to waiting and clears on removal") {
    State state;
    state.set_nodes({node("k-desk", "desk", true)});
    state.set_sessions({summary("a", "k-desk", 10, false), summary("b", "k-desk", 20, false)});
    CHECK((!state.is_running("a") && !state.shows_running("a")));
    state.set_running("a", true);
    CHECK((state.is_running("a") && state.shows_running("a")));
    CHECK((!state.is_running("b") && !state.shows_running("b")));
    auto a = summary("a", "k-desk", 30, false);
    a.title = "Updated a";
    state.upsert_session(a);
    state.set_nodes({node("k-desk", "desk", true)});
    CHECK((state.is_running("a") && state.shows_running("a")));
    a.waiting = true;
    a.updated = 35;
    state.upsert_session(a);
    CHECK((state.is_running("a") && !state.shows_running("a"))); // waiting wins over running
    a.waiting = false;
    a.updated = 40;
    state.upsert_session(a);
    CHECK((state.is_running("a") && state.shows_running("a")));
    state.set_running("b", true);
    state.set_running("a", false);
    CHECK((!state.is_running("a") && !state.shows_running("a")));
    state.remove_session("b");
    CHECK((!state.is_running("b") && !state.shows_running("b")));
    state.upsert_session(summary("b", "k-desk", 50, false));
    CHECK((!state.is_running("b") && !state.shows_running("b")));
    state.set_running("a", true);
    state.set_running("b", true);
    state.set_sessions({summary("b", "k-desk", 50, false)});
    CHECK((!state.is_running("a") && state.is_running("b")));
}

TEST_CASE("nodes name unknown keys by their first eight characters") {
    const Nodes nodes({node("k-desk", "desk", true)});
    CHECK(nodes.name_of("k-desk") == "desk");
    CHECK(nodes.name_of("0123456789abcdef") == "01234567");
    CHECK(nodes.name_of("short") == "short");
    CHECK(nodes.is_online("k-desk"));
    CHECK(!nodes.is_online("nope"));
}
