#include <doctest/doctest.h>

#include "loa/harness/kit/mapper_impl.hpp"
#include "support.hpp"

using namespace loa::harness::kit;
using namespace loa::core;
using loa::contracts::AdapterError;
using loa::contracts::Result;
using loa::contracts::SessionState;
using loa::harness::test::block_on;

namespace {

// Events: "end" ends the turn; "ask" opens a waiting decision; anything else is replied to.
class Mapper final : public EventMapper<std::string, std::string> {
  public:
    explicit Mapper(NodeId session) : ledger_(session) {}

    Result<std::vector<std::string>> apply(std::string ev, const DomWriter& w) override {
        if (ev == "end") {
            if (auto r = ledger_.end_turn(w, TurnStatus::Done); !r) return std::unexpected(AdapterError::Commit{r.error()});
            return std::vector<std::string>{};
        }
        if (ev == "ask") {
            auto d = w.insert(ledger_.turn()->node, ElementKind::Decision, Attrs{{"kind", DecisionKind::Approval}, {"status", DecisionStatus::Waiting}});
            ledger_.wait(*d);
            last_decision = *d;
            return std::vector<std::string>{};
        }
        return std::vector<std::string>{"ack:" + ev};
    }

    Result<std::optional<std::string>> answer(NodeId decision, const loa::contracts::Answer& answer, const DomWriter& w) override {
        (void)w.close(decision, DecisionStatus::Answered);
        return std::optional<std::string>("answer:" + answer.values.at(0));
    }

    const TurnLedger<std::string>& ledger() const override { return ledger_; }
    TurnLedger<std::string>& ledger_mut() override { return ledger_; }

    NodeId last_decision;

  private:
    TurnLedger<std::string> ledger_;
};

struct Log {
    std::mutex mutex;
    std::vector<std::string> lines;
    void push(std::string s) {
        std::scoped_lock lock(mutex);
        lines.push_back(std::move(s));
    }
};

class Ops final : public HarnessOps<std::string> {
  public:
    explicit Ops(std::shared_ptr<Log> log) : log_(std::move(log)) {}
    std::string_view harness() const override { return "fake"; }
    Awaitable<Result<>> send(loa::contracts::Prompt prompt) const override {
        log_->push("send:" + prompt.text);
        co_return Result<>{};
    }
    Awaitable<Result<>> reply(std::string reply) const override {
        log_->push(reply);
        co_return Result<>{};
    }
    Awaitable<Result<>> interrupt() const override {
        log_->push("interrupt");
        co_return Result<>{};
    }
    Awaitable<Result<>> shutdown() const override {
        log_->push("shutdown");
        co_return Result<>{};
    }

  private:
    std::shared_ptr<Log> log_;
};

using Session = DrivenSession<Mapper, Ops>;

} // namespace

TEST_CASE("kit: DrivenSession sends when idle, is Busy mid turn and Closed after shutdown") {
    loa::runtime::Executor exec(2);
    auto sink = std::make_shared<MemorySink>();
    DomWriter w(*sink);
    loa::contracts::SessionConfig cfg;
    cfg.session_id = "s";
    NodeId root = insert_root(w, "fake", cfg).value();
    auto log = std::make_shared<Log>();
    Session session(Mapper(root), Ops(log), sink);
    CHECK(session.harness() == "fake");
    CHECK(session.state() == SessionState::Idle);

    // Interrupt is a no-op when idle.
    REQUIRE(block_on(exec, session.interrupt()));
    CHECK(log->lines.empty());

    REQUIRE(block_on(exec, session.send({"hi", {}, "me"})));
    CHECK(session.state() == SessionState::Running);
    auto busy = block_on(exec, session.send({"again", {}, "me"}));
    REQUIRE_FALSE(busy);
    CHECK(busy.error().get<AdapterError::Busy>());

    REQUIRE(block_on(exec, session.feed("chunk")));
    REQUIRE(block_on(exec, session.interrupt()));
    REQUIRE(block_on(exec, session.feed("ask")));
    CHECK(session.state() == SessionState::Waiting);

    auto unsupported = block_on(exec, session.set_model({"m", std::nullopt}));
    REQUIRE_FALSE(unsupported);
    REQUIRE(unsupported.error().get<AdapterError::Unsupported>());
    CHECK(unsupported.error().get<AdapterError::Unsupported>()->what == "set_model");

    // wait_idle resolves once the turn ends.
    auto waiting = asio::co_spawn(exec.context(), session.wait_idle(), asio::use_future);
    CHECK(waiting.wait_for(std::chrono::milliseconds(50)) == std::future_status::timeout);
    REQUIRE(block_on(exec, session.feed("end")));
    REQUIRE(waiting.get());
    CHECK(session.state() == SessionState::Idle);

    REQUIRE(block_on(exec, session.shutdown()));
    CHECK(session.state() == SessionState::Closed);
    auto closed = block_on(exec, session.send({"late", {}, "me"}));
    REQUIRE_FALSE(closed);
    CHECK(closed.error().get<AdapterError::Closed>());
    auto wait_closed = block_on(exec, session.wait_idle());
    REQUIRE_FALSE(wait_closed);
    CHECK(wait_closed.error().get<AdapterError::Closed>());

    CHECK(log->lines == std::vector<std::string>{"send:hi", "ack:chunk", "interrupt", "shutdown"});
}

TEST_CASE("kit: DrivenSession answers a decision through the mapper and sends its reply") {
    loa::runtime::Executor exec(2);
    auto sink = std::make_shared<MemorySink>();
    DomWriter w(*sink);
    NodeId root = insert_root(w, "fake", {}).value();
    auto log = std::make_shared<Log>();
    Mapper mapper(root);
    Session session(std::move(mapper), Ops(log), sink);
    REQUIRE(block_on(exec, session.send({"hi", {}, "me"})));
    REQUIRE(block_on(exec, session.feed("ask")));
    CHECK(session.state() == SessionState::Waiting);
    // The decision is the only `<decision>` in the DOM.
    NodeId decision;
    const Dom dom = sink->dom();
    for (const Node& n : dom.iter())
        if (n.kind == ElementKind::Decision) decision = n.id;
    REQUIRE(block_on(exec, session.answer(decision, {{"yes"}, std::nullopt, "me"})));
    CHECK(session.state() == SessionState::Running);
    CHECK(log->lines.back() == "answer:yes");
    CHECK(sink->dom().get(decision)->str_attr(attr::STATUS) == "answered");
}
