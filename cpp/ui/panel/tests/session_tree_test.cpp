// Port of the `session_tree.rs` test: forks hang under their turns and cycles become roots.

#include <doctest/doctest.h>

#include <format>
#include <tuple>

#include "loa/ui/panel/session_tree.hpp"

using namespace loa::ui::panel::session_tree;
using loa::core::NodeId;
using loa::view::panel::TurnInfo;
namespace si = loa::view::panel::session_tree_intent;

namespace {

std::vector<TurnInfo> turns(std::initializer_list<std::tuple<std::uint64_t, std::uint64_t, const char*>> list) {
    std::vector<TurnInfo> out;
    for (const auto& [n, seq, l] : list) out.push_back({NodeId{n}, seq, l});
    return out;
}
SessionInfo session(std::string id, std::optional<std::pair<std::string, std::uint64_t>> parent, std::vector<TurnInfo> t) {
    SessionInfo s{.id = std::move(id), .turns = std::move(t)};
    if (parent) s.parent = parent->first, s.forked_at = parent->second;
    return s;
}
SessionInfo root() { return session("root", std::nullopt, turns({{10, 1, "one"}, {20, 5, "two"}, {30, 9, "three"}})); }
std::pair<std::string, std::uint64_t> p(std::string id, std::uint64_t at) { return {std::move(id), at}; }

// Rows indented by depth: `S id` (`*` current, `+` on path), `F label`, `T line`.
std::vector<std::string> outline(const State& s) {
    std::vector<std::string> out;
    for (const auto& r : s.rows()) {
        std::string body;
        if (const auto* x = std::get_if<kind::Session>(&r.kind)) body = std::format("S {}{}", x->id, x->current ? "*" : x->on_path ? "+" : "");
        else if (const auto* f = std::get_if<kind::ForkPoint>(&r.kind))
            body = "F " + (f->first_line ? std::string(*f->first_line) : std::format("at #{}", f->seq));
        else body = "T " + std::string(std::get<kind::Turn>(r.kind).first_line);
        out.push_back(std::string(2 * r.depth, ' ') + body);
    }
    return out;
}

} // namespace

TEST_CASE("forks hang under their turns and cycles become roots") {
    struct Case {
        std::vector<SessionInfo> family;
        std::string current;
        std::vector<std::string> want;
    };
    const std::vector<Case> cases{
        {{session("b", p("root", 9), {}), root(), session("a", p("root", 6), {}), session("x", p("root", 5), {}), session("z", p("root", 5), {})},
         "root",
         {"S root*", "  T one", "  T two", "  F two", "    S x", "    S z", "    S a", "  T three", "  F three", "    S b"}},
        {{root(), session("a", p("root", 5), turns({{40, 12, "a one"}, {50, 14, "a two"}})), session("b", p("a", 13), turns({{60, 20, "b one"}})),
          session("side", p("root", 1), {})},
         "b",
         {"S root+", "  F one", "    S side", "  F two", "    S a+", "      F a one", "        S b*", "          T b one"}},
        {{session("root", std::nullopt, turns({{10, 4, "one"}})), session("a", p("root", 2), {})}, "root", {"S root*", "  F at #2", "    S a", "  T one"}},
        {{root(), session("orphan", p("gone", 3), {})}, "root", {"S root*", "  T one", "  T two", "  T three", "S orphan"}},
        {{session("a", p("b", 1), {}), session("b", p("a", 1), {}), session("c", p("a", 1), {}), session("d", p("d", 1), {})}, "x", {"S a", "S b", "S c", "S d"}},
    };
    for (const auto& c : cases) {
        State s;
        s.set_family(c.family, c.current);
        CHECK(outline(s) == c.want);
    }
    State s;
    s.set_family({root(), session("orphan", p("gone", 3), {})}, "root");
    CHECK(s.update(msg::Rewind{2}).second == Event{si::Rewind{"root", NodeId{20}}});
    CHECK(s.update(msg::Fork{2}).second == Event{si::Fork{"root", NodeId{20}}});
    CHECK(s.update(msg::Open{4}).second == Event{si::Open{"orphan"}});
    CHECK_FALSE(s.update(msg::Open{0}).second);
    (void)s.update(msg::Toggle{0});
    CHECK(outline(s) == std::vector<std::string>{"S root*", "S orphan"});
    // Collapsed ids survive a new family that still has them.
    s.set_family({root()}, "root");
    CHECK(outline(s) == std::vector<std::string>{"S root*"});
}
