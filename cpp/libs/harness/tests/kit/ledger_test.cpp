#include <doctest/doctest.h>

#include "loa/harness/kit/ledger.hpp"

using namespace loa::harness::kit;
using namespace loa::core;
using loa::contracts::SessionState;

namespace {

loa::contracts::SessionConfig config() {
    loa::contracts::SessionConfig c;
    c.session_id = "s-1";
    c.node = "n-1";
    c.cwd = "/work";
    c.model = "m";
    c.mode = SessionMode::Plan;
    return c;
}

std::string status_of(const Dom& dom, NodeId id) { return std::string(dom.get(id)->str_attr(attr::STATUS).value_or("")); }

} // namespace

TEST_CASE("kit: insert_root writes the session attributes from the config") {
    MemorySink sink;
    DomWriter w = DomWriter(sink).at(1234);
    auto root = insert_root(w, "omp", config());
    REQUIRE(root);
    auto dom = sink.dom();
    const Node* n = dom.get(*root);
    REQUIRE(n);
    CHECK(n->kind == ElementKind::Session);
    CHECK(n->str_attr(attr::ID) == "s-1");
    CHECK(n->str_attr(attr::HARNESS) == "omp");
    CHECK(n->str_attr(attr::NODE) == "n-1");
    CHECK(n->int_attr(attr::CREATED) == 1234);
    CHECK(n->int_attr(attr::SCHEMA) == static_cast<std::int64_t>(SCHEMA_VERSION));
    CHECK(n->str_attr(attr::CWD) == "/work");
    CHECK(n->str_attr(attr::MODEL) == "m");
    CHECK(n->str_attr(attr::MODE) == "plan");
    CHECK_FALSE(n->attr(attr::PROJECT));
    CHECK_FALSE(n->attr(attr::EFFORT));
}

TEST_CASE("kit: TurnLedger tracks a turn and ends it, cancelling what is open") {
    MemorySink sink;
    DomWriter w(sink);
    NodeId session = insert_root(w, "omp", config()).value();
    TurnLedger<std::string> ledger(session);
    CHECK(ledger.state() == SessionState::Idle);

    loa::contracts::Prompt prompt{"hello", {}, "me"};
    NodeId turn = ledger.begin(w, prompt).value();
    CHECK(ledger.state() == SessionState::Running);
    {
        auto dom = sink.dom();
        auto messages = dom.children_of_kind(turn, ElementKind::Message);
        REQUIRE(messages.size() == 1);
        CHECK(messages[0]->text == "hello");
        CHECK(messages[0]->str_attr(attr::BY) == "me");
        CHECK(messages[0]->str_attr(attr::ROLE) == "user");
    }

    NodeId msg = w.insert(turn, ElementKind::Message, Attrs{{"role", Role::Assistant}, {"status", MessageStatus::Streaming}}).value();
    NodeId call = w.insert(turn, ElementKind::Call, Attrs{{"tool", "bash"}, {"status", CallStatus::Running}}).value();
    NodeId decision = w.insert(call, ElementKind::Decision, Attrs{{"kind", DecisionKind::Approval}, {"status", DecisionStatus::Waiting}}).value();
    ledger.open("m", msg);
    ledger.open("c", call);
    CHECK(ledger.get("m") == msg);
    ledger.wait(decision);
    CHECK(ledger.state() == SessionState::Waiting);
    ledger.answered(decision);
    CHECK(ledger.state() == SessionState::Running);
    ledger.wait(decision);
    CHECK(ledger.take("c") == call);
    CHECK_FALSE(ledger.take("c"));

    std::vector<std::string> seen;
    REQUIRE(ledger.end_turn(w, TurnStatus::Error, [&](const EndEvent<std::string>& ev) {
        if (auto* c = std::get_if<end_event::Child<std::string>>(&ev)) seen.push_back("child:" + *c->key);
        else if (std::get_if<end_event::Decision>(&ev)) seen.push_back("decision");
        else seen.push_back("turn:" + std::string(as_str(std::get<end_event::Turn>(ev).status)));
    }));
    CHECK(seen == std::vector<std::string>{"child:m", "decision", "turn:error"});
    auto dom = sink.dom();
    CHECK(status_of(dom, msg) == "error");
    CHECK(status_of(dom, decision) == "cancelled");
    CHECK(status_of(dom, turn) == "error");
    CHECK(dom.get(turn)->attr(attr::ENDED));
    CHECK(ledger.state() == SessionState::Idle);
    CHECK(ledger.end_turn(w, TurnStatus::Done));

    ledger.close();
    CHECK(ledger.state() == SessionState::Closed);
}

TEST_CASE("kit: TurnLedger::from_dom resumes the last open turn and its waiting decisions") {
    MemorySink sink;
    DomWriter w(sink);
    NodeId session = insert_root(w, "omp", config()).value();
    TurnLedger<std::string> first(session);
    NodeId t1 = first.begin_with(w, nullptr).value();
    REQUIRE(first.end_turn(w, TurnStatus::Done));
    NodeId t2 = first.begin_with(w, nullptr).value();
    NodeId call = w.insert(t2, ElementKind::Call, Attrs{{"tool", "bash"}, {"status", CallStatus::Running}}).value();
    NodeId waiting = w.insert(call, ElementKind::Decision, Attrs{{"kind", DecisionKind::Approval}, {"status", DecisionStatus::Waiting}}).value();
    (void)w.insert(call, ElementKind::Decision, Attrs{{"kind", DecisionKind::Approval}, {"status", DecisionStatus::Answered}}).value();
    (void)t1;

    auto resumed = TurnLedger<std::string>::from_dom(sink.dom());
    CHECK(resumed.session == session);
    REQUIRE(resumed.turn());
    CHECK(resumed.turn()->node == t2);
    CHECK(resumed.turn()->waiting == std::vector<NodeId>{waiting});
    CHECK(resumed.state() == SessionState::Waiting);

    REQUIRE(resumed.end_turn(w, TurnStatus::Cancelled));
    auto idle = TurnLedger<std::string>::from_dom(sink.dom());
    CHECK_FALSE(idle.turn());
    CHECK(idle.state() == SessionState::Idle);

    auto empty = TurnLedger<std::string>::from_dom(Dom());
    CHECK(empty.session == NodeId::DOCUMENT);
}
