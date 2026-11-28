// The `testing` checks accept a well-formed session and flag a broken one (port of `tests/dom.rs`).
#include <doctest/doctest.h>

#include "loa/harness/testing/testing.hpp"

using namespace loa::core;
using namespace loa::harness::testing;

namespace {

NodeId insert(WatchSink& sink, NodeId parent, ElementKind kind, Attrs attrs, std::string text) {
    return sink.commit_at(Draft::insert(parent, kind, std::move(attrs), std::move(text)), 30).value().node;
}

Attrs attrs(std::vector<std::pair<std::string_view, Value>> pairs) { return into_attrs(std::move(pairs)); }

NodeId session_with_turn(WatchSink& sink) {
    auto session = insert(sink, NodeId::DOCUMENT, ElementKind::Session,
                          attrs({{attr::ID, "s"}, {attr::HARNESS, "h"}, {attr::NODE, "n"}, {attr::CREATED, 10}, {attr::SCHEMA, SCHEMA_VERSION}}), "");
    auto turn = insert(sink, session, ElementKind::Turn, attrs({{attr::STATUS, TurnStatus::Running}}), "");
    insert(sink, turn, ElementKind::Message, attrs({{attr::ROLE, Role::User}, {attr::STATUS, MessageStatus::Done}}), "hi");
    return turn;
}

std::vector<Rule> rules(const WatchSink& sink) {
    std::vector<Rule> out;
    for (const auto& v : sink.inner.with_dom([](const Dom& d) { return check_dom(d); })) out.push_back(v.rule);
    return out;
}

} // namespace

TEST_CASE("testing: a well-formed session passes and a running turn is only live") {
    WatchSink sink;
    auto turn = session_with_turn(sink);
    CHECK(rules(sink) == std::vector<Rule>{Rule::NotTerminal});
    CHECK(sink.inner.with_dom([](const Dom& d) { return check_dom_in(d, Phase::Live); }).empty());
    auto call = insert(sink, turn, ElementKind::Call, attrs({{attr::TOOL, "bash"}, {attr::STATUS, CallStatus::Running}}), "");
    insert(sink, call, ElementKind::Input, {}, "{}");
    insert(sink, call, ElementKind::Result, attrs({{attr::IS_ERROR, false}}), "ok");
    REQUIRE(sink.commit_at(Draft::set(call, std::string(attr::STATUS), CallStatus::Done), 40));
    REQUIRE(sink.commit_at(Draft::set(turn, std::string(attr::STATUS), TurnStatus::Done), 50));
    CHECK(sink.seq() == sink.inner.patches().back().seq);
    CHECK(rules(sink).empty());
    CHECK(check_history(sink.inner.patches()).empty());
}

TEST_CASE("testing: a done call without input or result is flagged") {
    WatchSink sink;
    auto turn = session_with_turn(sink);
    insert(sink, turn, ElementKind::Call, attrs({{attr::TOOL, "bash"}, {attr::STATUS, CallStatus::Done}}), "");
    REQUIRE(sink.commit_at(Draft::set(turn, std::string(attr::STATUS), TurnStatus::Done), 50));
    CHECK(rules(sink) == std::vector<Rule>{Rule::CallInput, Rule::CallResult});
}

TEST_CASE("testing: history flags bad transitions and close order") {
    WatchSink sink;
    auto turn = session_with_turn(sink);
    auto call = insert(sink, turn, ElementKind::Call, attrs({{attr::TOOL, "bash"}, {attr::STATUS, CallStatus::Running}}), "");
    REQUIRE(sink.commit_at(Draft::set(turn, std::string(attr::STATUS), TurnStatus::Done), 50));
    REQUIRE(sink.commit_at(Draft::set(call, std::string(attr::STATUS), CallStatus::Pending), 60));
    const auto history = check_history(sink.inner.patches());
    REQUIRE(history.size() == 2);
    CHECK(history[0] == Violation{Rule::CloseOrder, call, "patch 5 ends the turn while this is open"});
    CHECK(history[1] == Violation{Rule::Transition, call, "patch 6 moves <call> from running to pending"});
    const auto live = sink.inner.with_dom([](const Dom& d) { return check_dom_in(d, Phase::Live); });
    REQUIRE_FALSE(live.empty());
    CHECK(live[0].rule == Rule::NotTerminal);
    CHECK(live[0].detail == "<call> is open under an ended turn");
    CHECK(check_dom(Dom{}) == std::vector<Violation>{{Rule::Root, std::nullopt, "the root isn't a <session>"}});
    CHECK(to_string(Rule::DecisionAnswer) == "DecisionAnswer");
}
