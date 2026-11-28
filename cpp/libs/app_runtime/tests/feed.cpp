#include <doctest/doctest.h>

#include <chrono>
#include <future>
#include <thread>

#include <asio/co_spawn.hpp>
#include <asio/io_context.hpp>
#include <asio/use_future.hpp>

#include "loa/app_runtime/feed.hpp"
#include "loa/app_runtime/lifecycle.hpp"
#include "loa/app_runtime/harnesses.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/harness/testing/scripted.hpp"
#include "loa/runtime/executor.hpp"

using namespace loa::app_runtime;

namespace {

template <typename T>
T block_on(loa::runtime::Executor& exec, asio::awaitable<T> task) {
    return asio::co_spawn(exec.context(), std::move(task), asio::use_future).get();
}

} // namespace

TEST_CASE("broadcast: a lagged receiver reports exactly how many items were skipped") {
    loa::runtime::Executor exec(1);
    auto [send, receiver] = broadcast<int>(2);
    std::optional<decltype(send)> sender(std::move(send));
    sender->send(1);
    sender->send(2);
    sender->send(3);
    sender->send(4);
    auto lag = block_on(exec, receiver.next());
    REQUIRE(lag);
    CHECK_FALSE(*lag);
    CHECK(lag->error() == 2);
    auto item = block_on(exec, receiver.next());
    REQUIRE(item);
    REQUIRE(*item);
    CHECK(**item == 3);
    CHECK(**block_on(exec, receiver.next()) == 4);
    sender.reset();
    CHECK_FALSE(block_on(exec, receiver.next()));
}

TEST_CASE("broadcast: receivers start at the current tail and do not share cursors") {
    loa::runtime::Executor exec(1);
    auto [sender, first] = broadcast<std::string>(1);
    sender.send("old");
    auto second = sender.subscribe();
    sender.send("new");
    auto lag = block_on(exec, first.next());
    REQUIRE(lag);
    CHECK(lag->error() == 1);
    CHECK(**block_on(exec, first.next()) == "new");
    CHECK(**block_on(exec, second.next()) == "new");
}

TEST_CASE("watch: changed reports unseen updates and ends after the sender is gone") {
    loa::runtime::Executor exec(1);
    auto [send, receiver] = watch<std::string>("before");
    std::optional<decltype(send)> sender(std::move(send));
    CHECK(receiver.borrow() == "before");
    sender->send("after");
    CHECK(block_on(exec, receiver.changed()));
    CHECK(receiver.borrow() == "after");
    sender->send("last");
    auto changed = block_on(exec, receiver.next());
    CHECK(changed.has_value());
    CHECK(receiver.borrow_and_update() == "last");
    sender.reset();
    CHECK_FALSE(block_on(exec, receiver.next()));
}

TEST_CASE("unbounded: all queued items arrive before the end") {
    loa::runtime::Executor exec(1);
    auto [send, receiver] = unbounded<int>();
    std::optional<decltype(send)> sender(std::move(send));
    CHECK(sender->send(1));
    CHECK(sender->send(2));
    sender.reset();
    CHECK(block_on(exec, receiver.next()) == 1);
    CHECK(block_on(exec, receiver.next()) == 2);
    CHECK_FALSE(block_on(exec, receiver.next()));
    OptionalSource<int> absent;
    CHECK_FALSE(block_on(exec, absent.next()));
}

TEST_CASE("harnesses: replace, probe, select roles and default") {
    Harnesses harnesses;
    auto one = std::make_shared<loa::harness::testing::ScriptedAdapter>("omp");
    auto two = std::make_shared<loa::harness::testing::ScriptedAdapter>("codex");
    Harness omp;
    omp.id = "omp";
    omp.label = "Omp";
    omp.traits.has_roles = true;
    harnesses.add(one, omp);
    Harness codex;
    codex.id = "codex";
    codex.label = "Codex";
    codex.available = true;
    harnesses.add(two, codex);
    CHECK_FALSE(harnesses.runnable("omp"));
    CHECK(harnesses.runnable("codex") == two);
    CHECK(harnesses.default_id() == "codex");
    CHECK(harnesses.roles_id() == "omp");
    CHECK(harnesses.label("missing") == "missing");
    CHECK_FALSE(harnesses.traits("missing").has_roles);
    harnesses.set_availability("omp", loa::contracts::Availability::ready("/bin/omp", "1.0"));
    CHECK(harnesses.runnable("omp") == one);
    CHECK(harnesses.default_id() == "omp");
    CHECK(harnesses.get("omp")->executable == std::filesystem::path("/bin/omp"));
    harnesses.set_availability("omp", loa::contracts::Availability{}); // Unknown retains readiness and executable.
    CHECK(harnesses.get("omp")->available);
    CHECK(harnesses.get("omp")->executable.has_value());
    Harness replacement = *harnesses.get("omp");
    replacement.label = "new label";
    harnesses.add(one, std::move(replacement));
    CHECK(harnesses.list.size() == 2);
    CHECK(harnesses.label("omp") == "new label");
    harnesses.set_availability("omp", loa::contracts::Availability::unavailable(
                                          loa::contracts::AvailabilityStatus::NotInstalled, "missing"));
    CHECK_FALSE(harnesses.runnable("omp"));
    CHECK(harnesses.default_id() == "codex");
}

TEST_CASE("ResumeError classifies unsupported and malformed formats") {
    using E = loa::contracts::AdapterError;
    CHECK(ResumeError::from(E(E::Unsupported{"scripted", "resume"})).kind == ResumeError::Kind::Unsupported);
    CHECK(ResumeError::from(E(E::Format{"invalid"})).kind == ResumeError::Kind::Unsupported);
    CHECK(ResumeError::from(E(E::Protocol{"crashed"})).kind == ResumeError::Kind::Failed);
}

TEST_CASE("waiting sources wake for an item from another thread") {
    using namespace std::chrono_literals;
    loa::runtime::Executor exec(1);
    auto [sender, receiver] = broadcast<std::string>(2);
    auto waiting = asio::co_spawn(exec.context(), receiver.next(), asio::use_future);
    std::this_thread::sleep_for(10ms);
    sender.send("ready");
    REQUIRE(waiting.wait_for(1s) == std::future_status::ready);
    CHECK(**waiting.get() == "ready");
}
