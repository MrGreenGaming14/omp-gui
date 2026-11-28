#pragma once

// Private: wake-ups for coroutines on a multi-threaded executor.

#include <asio.hpp>
#include <asio/experimental/concurrent_channel.hpp>

#include <memory>
#include <mutex>
#include <vector>

namespace loa::harness::kit::detail {

using Signal = asio::experimental::concurrent_channel<void(asio::error_code)>;

// Waiters park until the next `notify_all`; callers re-check their condition after
// waking. Register (`prepare`) under the lock that guards the condition, then `wait`.
class WaitList {
  public:
    // A wake-up slot for the calling coroutine; call while holding the condition's lock.
    std::shared_ptr<Signal> prepare(const asio::any_io_executor& ex) {
        auto signal = std::make_shared<Signal>(ex, 1);
        std::scoped_lock lock(mutex_);
        waiters_.push_back(signal);
        return signal;
    }

    static asio::awaitable<void> wait(std::shared_ptr<Signal> signal) {
        (void)co_await signal->async_receive(asio::as_tuple(asio::use_awaitable));
    }

    void notify_all() {
        std::vector<std::shared_ptr<Signal>> waiters;
        {
            std::scoped_lock lock(mutex_);
            waiters.swap(waiters_);
        }
        for (auto& w : waiters) w->try_send(asio::error_code{});
    }

  private:
    std::mutex mutex_;
    std::vector<std::shared_ptr<Signal>> waiters_;
};

} // namespace loa::harness::kit::detail
