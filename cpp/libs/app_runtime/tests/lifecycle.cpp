#include <doctest/doctest.h>

#include <atomic>
#include <filesystem>
#include <memory>
#include <string>
#include <vector>

#include <asio/co_spawn.hpp>
#include <asio/io_context.hpp>
#include <asio/use_future.hpp>

#include "loa/app_runtime/runtime.hpp"
#include "loa/harness/testing/scripted.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::app_runtime;
namespace c = loa::contracts;
namespace co = loa::core;
namespace b = loa::bus;

namespace {

constexpr const char* HARNESS = "scripted";

template <typename T>
T block_on(loa::runtime::Executor& executor, asio::awaitable<T> task) {
    return asio::co_spawn(executor.context(), std::move(task), asio::use_future).get();
}

class Nobody final : public b::Authorize {
  public:
    std::expected<void, b::Denied> authorize(const b::SessionId&, const b::NodeKey&, const co::Draft&,
                                             const co::Dom&) const override {
        return std::unexpected(b::Denied{"local only"});
    }
};

std::shared_ptr<loa::session::Library> library(const std::filesystem::path& dir) {
    b::NodeKey key;
    key.bytes.fill(1);
    return std::make_shared<loa::session::Library>(
        std::move(b::Store::make(dir).value()), std::make_shared<b::Bus>(), key, std::make_shared<Nobody>());
}

class CountedSession final : public c::HarnessSession {
  public:
    CountedSession(c::SessionPtr inner, std::shared_ptr<std::atomic_size_t> shutdowns,
                   std::shared_ptr<std::atomic_bool> fail_shutdown)
        : inner_(std::move(inner)), shutdowns_(std::move(shutdowns)), fail_shutdown_(std::move(fail_shutdown)) {}

    c::Awaitable<c::Result<>> send(c::Prompt prompt) const override { co_return co_await inner_->send(std::move(prompt)); }
    c::Awaitable<c::Result<>> interrupt() const override { co_return co_await inner_->interrupt(); }
    c::Awaitable<c::Result<>> answer(co::NodeId decision, c::Answer answer) const override {
        co_return co_await inner_->answer(decision, std::move(answer));
    }
    std::string_view harness() const override { return inner_->harness(); }
    c::SessionState state() const override { return inner_->state(); }
    c::Awaitable<c::Result<>> wait_idle() const override { co_return co_await inner_->wait_idle(); }
    c::Awaitable<c::Result<>> shutdown() const override {
        ++*shutdowns_;
        if (*fail_shutdown_) co_return std::unexpected(c::AdapterError(c::AdapterError::Protocol{"shutdown failed"}));
        co_return co_await inner_->shutdown();
    }

  private:
    c::SessionPtr inner_;
    std::shared_ptr<std::atomic_size_t> shutdowns_;
    std::shared_ptr<std::atomic_bool> fail_shutdown_;
};

// The Rust lifecycle test's `Counted` scripted harness.
class Counted final : public c::Adapter {
  public:
    explicit Counted(loa::harness::testing::ScriptedAdapter inner)
        : inner_(std::move(inner)), shutdowns_(std::make_shared<std::atomic_size_t>(0)),
          fail_shutdown_(std::make_shared<std::atomic_bool>(false)) {}
    std::string_view harness() const override { return inner_.harness(); }
    c::Capabilities capabilities() const override { return inner_.capabilities(); }
    c::Awaitable<c::Result<c::SessionPtr>> start(c::SessionConfig config,
                                                 std::shared_ptr<co::PatchSink> sink) const override {
        auto result = co_await inner_.start(std::move(config), std::move(sink));
        if (!result) co_return std::unexpected(result.error());
        co_return wrap(std::move(*result));
    }
    c::Awaitable<c::Result<c::SessionPtr>> resume(c::SessionConfig config, const co::Dom& dom,
                                                  std::shared_ptr<co::PatchSink> sink) const override {
        auto result = co_await inner_.resume(std::move(config), dom, std::move(sink));
        if (!result) co_return std::unexpected(result.error());
        co_return wrap(std::move(*result));
    }
    [[nodiscard]] std::size_t shutdowns() const { return *shutdowns_; }
    void fail_shutdown(bool fail) { *fail_shutdown_ = fail; }
    [[nodiscard]] loa::harness::testing::Seen seen() const { return inner_.seen(); }

  private:
    c::SessionPtr wrap(c::SessionPtr session) const {
        return std::make_unique<CountedSession>(std::move(session), shutdowns_, fail_shutdown_);
    }
    loa::harness::testing::ScriptedAdapter inner_;
    std::shared_ptr<std::atomic_size_t> shutdowns_;
    std::shared_ptr<std::atomic_bool> fail_shutdown_;
};

Runtime make_runtime(std::shared_ptr<loa::session::Library> lib, std::shared_ptr<Counted> adapter,
                     const std::filesystem::path& cwd) {
    Harnesses harnesses;
    Harness harness;
    harness.id = HARNESS;
    harness.label = "Scripted";
    harness.available = true;
    harness.program = HARNESS;
    harnesses.add(std::move(adapter), std::move(harness));
    Runtime runtime(RuntimeParts{std::move(lib), std::move(harnesses), "node", cwd, "project", std::nullopt});
    runtime.sessions_mut().harness = HARNESS;
    return runtime;
}

c::Prompt prompt(std::string text) { return {std::move(text), {}, "user"}; }

runtime_event::PrepareSession prepared(const Transition& transition) {
    REQUIRE(transition.effects.empty());
    REQUIRE(transition.events.size() == 1);
    auto* event = std::get_if<runtime_event::PrepareSession>(&transition.events.front());
    REQUIRE(event);
    CHECK(event->harness == HARNESS);
    return *event;
}

Completion run_one(loa::runtime::Executor& executor, Transition transition) {
    REQUIRE(transition.effects.size() == 1);
    return block_on(executor, transition.effects.front()());
}

std::pair<b::SessionId, Transition> start(Runtime& runtime, loa::runtime::Executor& executor,
                                           std::optional<c::Prompt> p = std::nullopt) {
    auto prep = prepared(runtime.dispatch(command::Start{std::move(p)}));
    b::SessionId id(prep.config.session_id);
    auto result = run_one(executor, runtime.dispatch(command::Prepared{prep.operation, std::move(prep.config)}));
    return {id, runtime.dispatch(command::Completed{std::move(result)})};
}

std::vector<RuntimeEvent> deliver_feed(Runtime& runtime, b::SessionId id, Feed feed) {
    auto batch = runtime.begin_feed(std::move(id), std::move(feed));
    std::vector<RuntimeEvent> events;
    while (auto transition = runtime.advance_feed(batch)) {
        REQUIRE(transition->effects.empty());
        events.insert(events.end(), std::make_move_iterator(transition->events.begin()),
                      std::make_move_iterator(transition->events.end()));
    }
    return events;
}

template <typename T>
bool is(const RuntimeEvent& event) { return std::holds_alternative<T>(event); }

template <typename T>
bool session_is(const RuntimeEvent& event) {
    auto* outer = std::get_if<runtime_event::Session>(&event);
    return outer && std::holds_alternative<T>(outer->event);
}

std::shared_ptr<b::SessionWriter> writer(const Runtime& runtime, const b::SessionId& id) {
    return runtime.sessions().get(id.as_str())->writer();
}

} // namespace

TEST_CASE("start sends the prompt once and reopens from the journal") {
    auto dir = loa::runtime::TempDir::create().value();
    loa::runtime::Executor executor(2);
    auto adapter = std::make_shared<Counted>(loa::harness::testing::ScriptedAdapter(HARNESS));
    auto runtime = make_runtime(library(dir.path()), adapter, dir.path());

    auto [id, started] = start(runtime, executor, prompt("hello"));
    CHECK(started.effects.empty());
    REQUIRE(started.events.size() == 3);
    CHECK(session_is<session_event::Activated>(started.events[0]));
    CHECK(is<runtime_event::SessionStarted>(started.events[1]));
    auto* requested = std::get_if<runtime_event::SendRequested>(&started.events[2]);
    REQUIRE(requested);
    CHECK(requested->id == id);
    CHECK(runtime.sessions().is_active(id.as_str()));
    CHECK_FALSE(runtime.sessions().starting());

    auto sent = runtime.dispatch(command::Send{requested->prompt});
    REQUIRE(sent.events.size() == 1);
    CHECK(is<runtime_event::TurnStarted>(sent.events.front()));
    auto done = run_one(executor, std::move(sent));
    CHECK(std::holds_alternative<completion::Done>(done));
    CHECK(runtime.dispatch(command::Completed{std::move(done)}).events.empty());
    CHECK(adapter->seen().prompts == std::vector<std::string>{"hello"});

    const co::Dom written = writer(runtime, id)->with_dom([](const co::Dom& dom) { return dom; });
    CHECK(written.seq() > 0);
    auto closed = runtime.dispatch(command::Close{id, false});
    REQUIRE(closed.events.size() == 2);
    CHECK(session_is<session_event::Activated>(closed.events[0]));
    CHECK(session_is<session_event::Closed>(closed.events[1]));
    CHECK(std::holds_alternative<completion::ShutDown>(run_one(executor, std::move(closed))));
    CHECK(adapter->shutdowns() == 1);

    auto opened = runtime.dispatch(command::Open{id});
    REQUIRE(opened.events.size() == 1);
    CHECK(session_is<session_event::Activated>(opened.events[0]));
    CHECK(runtime.sessions().get(id.as_str())->dom() == written);
    CHECK(adapter->seen().prompts.size() == 1);
}

TEST_CASE("a start completing after close shuts its harness down") {
    auto dir = loa::runtime::TempDir::create().value();
    loa::runtime::Executor executor(2);
    auto adapter = std::make_shared<Counted>(loa::harness::testing::ScriptedAdapter(HARNESS));
    auto runtime = make_runtime(library(dir.path()), adapter, dir.path());

    auto prep = prepared(runtime.dispatch(command::Start{prompt("hello")}));
    b::SessionId id(prep.config.session_id);
    auto completed = run_one(executor, runtime.dispatch(command::Prepared{prep.operation, std::move(prep.config)}));
    CHECK(std::get<completion::Started>(completed).result.has_value());
    auto closed = runtime.dispatch(command::Close{id, true});
    CHECK(closed.effects.empty());
    REQUIRE(closed.events.size() == 1);
    CHECK(session_is<session_event::Closed>(closed.events[0]));

    auto late = runtime.dispatch(command::Completed{std::move(completed)});
    CHECK(late.events.empty());
    CHECK(std::holds_alternative<completion::ShutDown>(run_one(executor, std::move(late))));
    CHECK(adapter->shutdowns() == 1);
    CHECK_FALSE(runtime.sessions().active_id());
    CHECK(runtime.sessions().get(id.as_str()) == nullptr);
    CHECK_FALSE(runtime.sessions().starting());
    CHECK(adapter->seen().prompts.empty());
}

TEST_CASE("a failed shutdown of a closed session leaves the active turn busy") {
    auto dir = loa::runtime::TempDir::create().value();
    loa::runtime::Executor executor(2);
    auto adapter = std::make_shared<Counted>(loa::harness::testing::ScriptedAdapter(HARNESS));
    auto runtime = make_runtime(library(dir.path()), adapter, dir.path());
    auto [closing, first] = start(runtime, executor);
    auto [active, second] = start(runtime, executor);
    CHECK(runtime.sessions().is_active(active.as_str()));
    auto sent = runtime.dispatch(command::Send{prompt("hello")});
    REQUIRE(sent.effects.size() == 1);
    CHECK(runtime.sessions().active()->is_busy());

    adapter->fail_shutdown(true);
    auto closed = runtime.dispatch(command::Close{closing, false});
    REQUIRE(closed.events.size() == 1);
    CHECK(session_is<session_event::Closed>(closed.events[0]));
    auto shut = run_one(executor, std::move(closed));
    CHECK_FALSE(std::get<completion::ShutDown>(shut).result);
    auto failed = runtime.dispatch(command::Completed{std::move(shut)});
    REQUIRE(failed.events.size() == 1);
    CHECK(is<runtime_event::Notice>(failed.events[0]));
    CHECK(runtime.sessions().is_active(active.as_str()));
    CHECK(runtime.sessions().active()->is_busy());
}

TEST_CASE("a failed send ends the active turn") {
    auto dir = loa::runtime::TempDir::create().value();
    loa::runtime::Executor executor(2);
    auto adapter = std::make_shared<Counted>(loa::harness::testing::ScriptedAdapter(HARNESS));
    auto runtime = make_runtime(library(dir.path()), adapter, dir.path());
    auto [id, started] = start(runtime, executor);
    auto sent = runtime.dispatch(command::Send{prompt("hello")});
    CHECK(is<runtime_event::TurnStarted>(sent.events.front()));
    CHECK(runtime.sessions().active()->is_busy());
    auto failed = runtime.dispatch(command::Completed{completion::Done{std::unexpected(std::string("the harness went away"))}});
    REQUIRE(failed.events.size() == 2);
    CHECK(is<runtime_event::Notice>(failed.events[0]));
    CHECK(is<runtime_event::TurnEnded>(failed.events[1]));
    CHECK_FALSE(runtime.sessions().active()->is_busy());
    auto late = runtime.dispatch(command::Completed{completion::Done{std::unexpected(std::string("late"))}});
    REQUIRE(late.events.size() == 1);
    CHECK(is<runtime_event::Notice>(late.events[0]));
}

TEST_CASE("a second prepared launches nothing") {
    auto dir = loa::runtime::TempDir::create().value();
    loa::runtime::Executor executor(2);
    auto adapter = std::make_shared<Counted>(loa::harness::testing::ScriptedAdapter(HARNESS));
    auto runtime = make_runtime(library(dir.path()), adapter, dir.path());
    auto prep = prepared(runtime.dispatch(command::Start{std::nullopt}));
    auto first = runtime.dispatch(command::Prepared{prep.operation, prep.config});
    REQUIRE(first.effects.size() == 1);
    auto second = runtime.dispatch(command::Prepared{prep.operation, prep.config});
    CHECK(second.effects.empty());
    CHECK(second.events.empty());
    auto unknown = runtime.dispatch(command::Prepared{prep.operation + 1, prep.config});
    CHECK(unknown.effects.empty());
    auto completion = run_one(executor, std::move(first));
    auto started = runtime.dispatch(command::Completed{std::move(completion)});
    REQUIRE(started.events.size() == 2);
    CHECK(session_is<session_event::Activated>(started.events[0]));
    CHECK(is<runtime_event::SessionStarted>(started.events[1]));
    CHECK(adapter->seen().configs.size() == 1);
}

TEST_CASE("resume errors continue or notify") {
    auto dir = loa::runtime::TempDir::create().value();
    auto old = dir.path() / "old";
    auto newer = dir.path() / "new";
    std::filesystem::create_directories(old);
    std::filesystem::create_directories(newer);
    loa::runtime::Executor executor(2);
    auto adapter = std::make_shared<Counted>(std::move(loa::harness::testing::ScriptedAdapter(HARNESS).no_resume()));
    auto runtime = make_runtime(library(dir.path() / "library"), adapter, old);
    auto [id, started] = start(runtime, executor);
    auto closed = runtime.dispatch(command::Close{id, false});
    (void)run_one(executor, std::move(closed));
    runtime.dispatch(command::Open{id});
    CHECK_FALSE(runtime.sessions().active()->harness());
    runtime.set_active("other", newer);

    auto prep = prepared(runtime.dispatch(command::Send{prompt("again")}));
    CHECK(prep.config.cwd == old);
    auto completion = run_one(executor, runtime.dispatch(command::Prepared{prep.operation, std::move(prep.config)}));
    CHECK(std::get<completion::Resumed>(completion).result.error().kind == ResumeError::Kind::Unsupported);
    auto unsupported = runtime.dispatch(command::Completed{std::move(completion)});
    REQUIRE(unsupported.events.size() == 2);
    CHECK(is<runtime_event::Notice>(unsupported.events[0]));
    auto* continued = std::get_if<runtime_event::ContinueRequested>(&unsupported.events[1]);
    REQUIRE(continued);
    CHECK(continued->folder == old);
    CHECK(continued->prompt.text == "again");

    prep = prepared(runtime.dispatch(command::Send{prompt("once more")}));
    (void)runtime.dispatch(command::Prepared{prep.operation, prep.config});
    auto failed = runtime.dispatch(command::Completed{completion::Resumed{
        prep.operation, id, std::unexpected(ResumeError{ResumeError::Kind::Failed, "boom"}), prompt("once more")}});
    REQUIRE(failed.events.size() == 2);
    CHECK(is<runtime_event::Notice>(failed.events[0]));
    CHECK(is<runtime_event::ResumeFailed>(failed.events[1]));
    CHECK_FALSE(runtime.sessions().starting());

    // The missing native history is special: it continues instead of dropping the prompt.
    prep = prepared(runtime.dispatch(command::Send{prompt("again")}));
    (void)runtime.dispatch(command::Prepared{prep.operation, prep.config});
    auto missing = runtime.dispatch(command::Completed{completion::Resumed{
        prep.operation, id, std::unexpected(ResumeError{ResumeError::Kind::Failed, "native file not found"}), prompt("again")}});
    REQUIRE(missing.events.size() == 2);
    CHECK(is<runtime_event::ContinueRequested>(missing.events[1]));
}

TEST_CASE("feed skips stale patches merges appends resyncs and ends") {
    auto dir = loa::runtime::TempDir::create().value();
    loa::runtime::Executor executor(2);
    auto adapter = std::make_shared<Counted>(loa::harness::testing::ScriptedAdapter(HARNESS));
    auto runtime = make_runtime(library(dir.path()), adapter, dir.path());
    auto [id, started] = start(runtime, executor);
    auto sink = writer(runtime, id);
    const auto root = sink->with_dom([](const co::Dom& dom) { return dom.root()->id; });
    const auto turn = sink->commit(co::Draft::insert(root, co::ElementKind::Turn,
                                  {{std::string(co::attr::STATUS), co::Value(co::TurnStatus::Done)}})).value().node;
    WriterFeed feed(sink, b::Policy::coalescing(3));
    auto synced = block_on(executor, feed.next());
    REQUIRE(synced);
    auto events = deliver_feed(runtime, id, *synced);
    REQUIRE(events.size() == 1);
    CHECK(session_is<session_event::Loaded>(events[0]));
    CHECK(deliver_feed(runtime, id, *synced).empty());

    const auto message = sink->commit(co::Draft::insert(turn, co::ElementKind::Message,
        {{std::string(co::attr::ROLE), co::Value(co::Role::Assistant)}})).value().node;
    for (const char* text : {"one ", "two ", "three"}) (void)sink->commit(co::Draft::append(message, text)).value();
    auto batch = block_on(executor, feed.next());
    REQUIRE(batch);
    auto* queued = std::get_if<feed::Events>(&*batch);
    REQUIRE(queued);
    REQUIRE(queued->events.size() == 2);
    CHECK(std::holds_alternative<b::event::Patch>(queued->events[0]));
    CHECK(std::holds_alternative<b::Appends>(queued->events[1]));
    events = deliver_feed(runtime, id, *batch);
    REQUIRE(events.size() == 2);
    CHECK(session_is<session_event::Patch>(events[0]));
    CHECK(session_is<session_event::Patch>(events[1]));
    CHECK(deliver_feed(runtime, id, *batch).empty());
    CHECK(runtime.sessions().get(id.as_str())->dom().get(message)->text == "one two three");
    CHECK(runtime.sessions().get(id.as_str())->dom().seq() == sink->seq());

    for (int i = 0; i < 5; ++i)
        (void)sink->commit(co::Draft::insert(turn, co::ElementKind::Message,
            {{std::string(co::attr::ROLE), co::Value(co::Role::User)}}, "hi")).value();
    bool resynced = false;
    for (int i = 0; i < 8 && !resynced; ++i) {
        auto next = block_on(executor, feed.next());
        REQUIRE(next);
        resynced = std::holds_alternative<feed::Synced>(*next);
        events = deliver_feed(runtime, id, std::move(*next));
    }
    REQUIRE(resynced);
    REQUIRE(events.size() == 1);
    CHECK(session_is<session_event::Loaded>(events[0]));
    CHECK(runtime.sessions().get(id.as_str())->dom() == sink->with_dom([](const co::Dom& d) { return d; }));
    sink->close();
    auto ended = block_on(executor, feed.next());
    REQUIRE(ended);
    CHECK(std::holds_alternative<feed::Ended>(*ended));
    CHECK(deliver_feed(runtime, id, std::move(*ended)).empty());
    CHECK_FALSE(block_on(executor, feed.next()));
}

TEST_CASE("streamed appends arrive in few batches") {
    auto dir = loa::runtime::TempDir::create().value();
    loa::runtime::Executor executor(2);
    auto adapter = std::make_shared<Counted>(loa::harness::testing::ScriptedAdapter(HARNESS));
    auto runtime = make_runtime(library(dir.path()), adapter, dir.path());
    auto [id, started] = start(runtime, executor);
    auto sink = writer(runtime, id);
    const auto root = sink->with_dom([](const co::Dom& dom) { return dom.root()->id; });
    const auto turn = sink->commit(co::Draft::insert(root, co::ElementKind::Turn)).value().node;
    const auto message = sink->commit(co::Draft::insert(turn, co::ElementKind::Message,
        {{std::string(co::attr::ROLE), co::Value("assistant")}})).value().node;
    WriterFeed feed(sink, b::Policy::coalescing(4096));
    auto synced = block_on(executor, feed.next());
    REQUIRE(synced);
    auto mirror = *std::get<feed::Synced>(*synced).dom;
    for (int i = 0; i < 500; ++i) (void)sink->commit(co::Draft::append(message, "token ")).value();
    int batches = 0;
    while (mirror.seq() < sink->seq()) {
        auto next = block_on(executor, feed.next());
        REQUIRE(next);
        auto* events = std::get_if<feed::Events>(&*next);
        REQUIRE(events);
        ++batches;
        for (auto& event : events->events) REQUIRE(b::apply_to(std::move(event), mirror).value());
    }
    CHECK(batches <= 3);
    CHECK(mirror.get(message)->text == sink->with_dom(
        [message](const co::Dom& dom) { return dom.get(message)->text; }));
}

TEST_CASE("a batch keeps each patch before its event and reacts to the turn once") {
    auto dir = loa::runtime::TempDir::create().value();
    loa::runtime::Executor executor(2);
    auto adapter = std::make_shared<Counted>(loa::harness::testing::ScriptedAdapter(HARNESS));
    auto runtime = make_runtime(library(dir.path()), adapter, dir.path());
    auto [id, started] = start(runtime, executor);
    auto sink = writer(runtime, id);
    const auto root = sink->with_dom([](const co::Dom& dom) { return dom.root()->id; });
    WriterFeed feed(sink, b::Policy::coalescing(4096));
    deliver_feed(runtime, id, *block_on(executor, feed.next()));
    const auto turn = sink->commit(co::Draft::insert(root, co::ElementKind::Turn,
        {{std::string(co::attr::STATUS), co::Value(co::TurnStatus::Running)}})).value().node;
    (void)sink->commit(co::Draft::insert(turn, co::ElementKind::Message,
        {{std::string(co::attr::ROLE), co::Value(co::Role::User)}}, "Plan the release")).value();
    auto batch = runtime.begin_feed(id, *block_on(executor, feed.next()));
    std::vector<RuntimeEvent> events;
    while (auto transition = runtime.advance_feed(batch)) {
        for (const auto& event : transition->events) {
            if (auto* outer = std::get_if<runtime_event::Session>(&event)) {
                if (auto* patch = std::get_if<session_event::Patch>(&outer->event);
                    patch && patch->patch->node == turn && std::holds_alternative<co::op::Insert>(patch->patch->op))
                    CHECK(runtime.sessions().active()->dom().children(turn).empty());
            }
        }
        events.insert(events.end(), std::make_move_iterator(transition->events.begin()),
                      std::make_move_iterator(transition->events.end()));
    }
    REQUIRE(events.size() == 6);
    CHECK(is<runtime_event::StatusChanged>(events[0]));
    CHECK(session_is<session_event::Patch>(events[1]));
    CHECK(session_is<session_event::TurnStarted>(events[2]));
    CHECK(is<runtime_event::SummaryChanged>(events[3]));
    CHECK(session_is<session_event::Patch>(events[4]));
    CHECK(is<runtime_event::TurnStarted>(events[5]));
    CHECK(runtime.sessions().active()->summary()->title == "Plan the release");
    CHECK(runtime.sessions().active()->is_busy());

    (void)sink->commit(co::Draft::set(turn, std::string(co::attr::ENDED), co::Value(5))).value();
    (void)sink->commit(co::Draft::set(turn, std::string(co::attr::STATUS), co::Value(co::TurnStatus::Done))).value();
    events = deliver_feed(runtime, id, *block_on(executor, feed.next()));
    REQUIRE_FALSE(events.empty());
    CHECK(is<runtime_event::TurnEnded>(events.back()));
    CHECK_FALSE(runtime.sessions().active()->is_busy());
}

TEST_CASE("a prompt that cannot start a session comes back") {
    auto dir = loa::runtime::TempDir::create().value();
    auto adapter = std::make_shared<Counted>(loa::harness::testing::ScriptedAdapter(HARNESS));
    auto runtime = make_runtime(library(dir.path()), adapter, dir.path());
    (void)prepared(runtime.dispatch(command::Start{std::nullopt}));
    auto again = runtime.dispatch(command::Start{prompt("second")});
    REQUIRE(again.events.size() == 2);
    CHECK(is<runtime_event::Notice>(again.events[0]));
    CHECK(std::get<runtime_event::PromptReturned>(again.events[1]).prompt.text == "second");
    CHECK(runtime.dispatch(command::Start{std::nullopt}).events.size() == 1);
    auto other = make_runtime(library(dir.path() / "other"), adapter, dir.path());
    other.sessions_mut().harness.reset();
    auto none = other.dispatch(command::Start{prompt("hello")});
    REQUIRE(none.events.size() == 2);
    CHECK(is<runtime_event::Notice>(none.events[0]));
    CHECK(is<runtime_event::PromptReturned>(none.events[1]));
}

TEST_CASE("background turn changes emit status without active turn events") {
    auto dir = loa::runtime::TempDir::create().value();
    loa::runtime::Executor executor(2);
    auto adapter = std::make_shared<Counted>(loa::harness::testing::ScriptedAdapter(HARNESS));
    auto runtime = make_runtime(library(dir.path()), adapter, dir.path());
    auto [first, a] = start(runtime, executor);
    auto [second, b] = start(runtime, executor);
    auto sink = writer(runtime, first);
    const auto root = sink->with_dom([](const co::Dom& dom) { return dom.root()->id; });
    WriterFeed feed(sink, b::Policy::coalescing(4096));
    deliver_feed(runtime, first, *block_on(executor, feed.next()));
    const auto turn = sink->commit(co::Draft::insert(root, co::ElementKind::Turn,
        {{std::string(co::attr::STATUS), co::Value(co::TurnStatus::Running)}})).value().node;
    auto events = deliver_feed(runtime, first, *block_on(executor, feed.next()));
    CHECK(std::ranges::any_of(events, is<runtime_event::StatusChanged>));
    CHECK(std::ranges::none_of(events, is<runtime_event::TurnStarted>));
    CHECK(std::ranges::none_of(events, is<runtime_event::TurnEnded>));
    CHECK(runtime.sessions().get(first.as_str())->is_busy());
    CHECK(runtime.sessions().is_active(second.as_str()));
    (void)sink->commit(co::Draft::set(turn, std::string(co::attr::STATUS), co::Value(co::TurnStatus::Done))).value();
    events = deliver_feed(runtime, first, *block_on(executor, feed.next()));
    CHECK(std::ranges::any_of(events, is<runtime_event::StatusChanged>));
    CHECK(std::ranges::none_of(events, is<runtime_event::TurnStarted>));
    CHECK(std::ranges::none_of(events, is<runtime_event::TurnEnded>));
    CHECK_FALSE(runtime.sessions().get(first.as_str())->is_busy());
}

TEST_CASE("access change restarts immediately when idle and after the turn when busy") {
    auto dir = loa::runtime::TempDir::create().value();
    loa::runtime::Executor executor(2);
    auto adapter = std::make_shared<Counted>(loa::harness::testing::ScriptedAdapter(HARNESS));
    auto runtime = make_runtime(library(dir.path()), adapter, dir.path());
    auto [first, started] = start(runtime, executor);
    auto idle = runtime.dispatch(command::SetAccess{"write"});
    REQUIRE(idle.events.size() == 1);
    REQUIRE(idle.effects.size() == 1);
    CHECK_FALSE(runtime.sessions().active()->harness());
    CHECK(std::holds_alternative<completion::Done>(run_one(executor, std::move(idle))));
    CHECK(adapter->shutdowns() == 1);
    CHECK(runtime.access() == "write");
    // The next send resumes using the newly selected mode.
    auto prep = prepared(runtime.dispatch(command::Send{prompt("next")}));
    CHECK(prep.resume);
    CHECK(prep.config.approval_mode == "write");
    auto resumed = run_one(executor, runtime.dispatch(command::Prepared{prep.operation, std::move(prep.config)}));
    auto requested = runtime.dispatch(command::Completed{std::move(resumed)});
    REQUIRE(requested.events.size() == 1);
    CHECK(is<runtime_event::SendRequested>(requested.events[0]));
    auto sending = runtime.dispatch(command::Send{prompt("turn")});
    CHECK(runtime.sessions().active()->is_busy());
    auto delayed = runtime.dispatch(command::SetAccess{"yolo"});
    REQUIRE(delayed.events.size() == 1);
    CHECK(delayed.effects.empty());
    CHECK(runtime.restart_for_access());
    CHECK(runtime.sessions().active()->harness() != nullptr);
    auto sink = writer(runtime, first);
    auto root = sink->with_dom([](const co::Dom& dom) { return dom.root()->id; });
    auto turn = sink->commit(co::Draft::insert(root, co::ElementKind::Turn,
        {{std::string(co::attr::STATUS), co::Value(co::TurnStatus::Running)}})).value().node;
    // The feed sees the turn end; only then does the runtime detach the harness.
    deliver_feed(runtime, first, Feed(feed::Synced{std::make_shared<co::Dom>(
        sink->with_dom([](const co::Dom& dom) { return dom; }))}));
    auto done_patch = sink->commit(co::Draft::set(turn, std::string(co::attr::STATUS),
                                                  co::Value(co::TurnStatus::Done))).value();
    auto batch = runtime.begin_feed(first, Feed(feed::Events{{b::event::Patch{
        std::make_shared<const co::Patch>(std::move(done_patch))}}}));
    auto status = runtime.advance_feed(batch);
    REQUIRE(status);
    CHECK(is<runtime_event::StatusChanged>(status->events.front()));
    auto ended = runtime.advance_feed(batch);
    REQUIRE(ended);
    REQUIRE(ended->events.size() == 1);
    CHECK(is<runtime_event::TurnEnded>(ended->events[0]));
    REQUIRE(ended->effects.size() == 1);
    CHECK_FALSE(runtime.restart_for_access());
    CHECK_FALSE(runtime.sessions().active()->harness());
    CHECK(std::holds_alternative<completion::Done>(run_one(executor, std::move(*ended))));
    CHECK(adapter->shutdowns() == 2);
}

TEST_CASE("spawn prepares its full config and activates the session it creates") {
    auto dir = loa::runtime::TempDir::create().value();
    loa::runtime::Executor executor(2);
    auto adapter = std::make_shared<Counted>(loa::harness::testing::ScriptedAdapter(HARNESS));
    auto lib = library(dir.path());
    auto runtime = make_runtime(lib, adapter, dir.path());
    b::SessionId id("area-created");
    SpawnStart spawn_start = [lib, adapter](c::SessionConfig config)
        -> c::Awaitable<std::expected<HarnessPtr, std::string>> {
        auto live = lib->create(config.session_id);
        if (!live) co_return std::unexpected(live.error().to_string());
        auto started = co_await adapter->start(std::move(config), live->writer);
        if (!started) co_return std::unexpected(started.error().message());
        co_return HarnessPtr(std::move(*started));
    };
    auto prep = prepared(runtime.dispatch(command::Spawn{id, HARNESS, std::move(spawn_start), prompt("hello")}));
    CHECK(prep.config.session_id == id.as_str());
    CHECK_FALSE(prep.resume);
    auto completed = run_one(executor, runtime.dispatch(command::Prepared{prep.operation, std::move(prep.config)}));
    REQUIRE(std::get<completion::Spawned>(completed).result);
    auto activated = runtime.dispatch(command::Completed{std::move(completed)});
    REQUIRE(activated.events.size() == 3);
    CHECK(session_is<session_event::Activated>(activated.events[0]));
    CHECK(is<runtime_event::SessionStarted>(activated.events[1]));
    CHECK(std::get<runtime_event::SendRequested>(activated.events[2]).prompt.text == "hello");
    CHECK(runtime.sessions().is_active(id.as_str()));
}

TEST_CASE("recents keeps sessions with turns and deletes empty journals only after close") {
    auto dir = loa::runtime::TempDir::create().value();
    loa::runtime::Executor executor(2);
    auto adapter = std::make_shared<Counted>(loa::harness::testing::ScriptedAdapter(HARNESS));
    auto lib = library(dir.path());
    auto runtime = make_runtime(lib, adapter, dir.path());
    auto [kept, first] = start(runtime, executor);
    auto sink = writer(runtime, kept);
    auto root = sink->with_dom([](const co::Dom& dom) { return dom.root()->id; });
    (void)sink->commit(co::Draft::insert(root, co::ElementKind::Turn)).value();
    auto [empty, second] = start(runtime, executor);
    auto recent = recents(*lib);
    REQUIRE(recent.problems.empty());
    REQUIRE(recent.summaries.size() == 1);
    CHECK(recent.summaries[0].id == kept.as_str());
    CHECK(recent.empty == std::vector<std::string>{empty.str()});
    // An open session is not deleted from under its writer.
    CHECK(runtime.delete_empty(recent.empty).empty());
    CHECK(lib->store().inspect(empty.as_str()));
    auto closed = runtime.dispatch(command::Close{empty, false});
    (void)run_one(executor, std::move(closed));
    CHECK(runtime.delete_empty(recent.empty).empty());
    CHECK_FALSE(lib->store().inspect(empty.as_str()));
}
