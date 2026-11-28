#include <doctest/doctest.h>

#include <asio.hpp>

#include <array>
#include <atomic>
#include <thread>
#include <vector>

#include "loa/runtime/executor.hpp"

TEST_CASE("1000 coroutines spawned from four threads each run exactly once; stop ends run") {
    loa::runtime::Executor exec(3);
    constexpr int kThreads = 4, kPer = 250, kTotal = kThreads * kPer;
    std::array<std::atomic<int>, kTotal> hits{};
    std::atomic<int> done{0};

    std::vector<std::jthread> spawners;
    for (int t = 0; t < kThreads; ++t) {
        spawners.emplace_back([&, t] {
            for (int i = 0; i < kPer; ++i) {
                const int id = t * kPer + i;
                exec.spawn([](std::atomic<int>& hit, std::atomic<int>& done, loa::runtime::Executor& exec) -> asio::awaitable<void> {
                    co_await asio::post(co_await asio::this_coro::executor, asio::use_awaitable);
                    hit.fetch_add(1);
                    if (done.fetch_add(1) + 1 == kTotal) exec.stop();
                }(hits[id], done, exec));
            }
        });
    }
    exec.run(); // returns once the last coroutine calls stop()
    spawners.clear();

    CHECK(exec.stopped());
    CHECK(done.load() == kTotal);
    for (const auto& h : hits) CHECK(h.load() == 1);
}

TEST_CASE("stop before run makes run return immediately") {
    loa::runtime::Executor exec;
    exec.stop();
    exec.run();
    CHECK(exec.stopped());
}
