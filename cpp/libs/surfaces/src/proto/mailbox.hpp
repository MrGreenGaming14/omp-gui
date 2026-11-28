#pragma once

// Private helpers on Asio: a wake-up signal and an unbounded multi-producer,
// single-consumer queue (the Rust `mpsc::unbounded_channel`), plus detached
// pumps that end when their owner goes away.

#include <asio.hpp>
#include <asio/experimental/concurrent_channel.hpp>

#include <deque>
#include <memory>
#include <mutex>
#include <optional>

namespace loa::surfaces::proto::detail {

// Level-triggered wake-up: `notify` never blocks and is remembered until the next `wait`.
class Signal {
  public:
    explicit Signal(const asio::any_io_executor& ex) : channel_(ex, 1) {}
    void notify() { channel_.try_send(asio::error_code{}); }
    // False when the wait was cancelled.
    asio::awaitable<bool> wait() {
        auto [ec] = co_await channel_.async_receive(asio::as_tuple(asio::use_awaitable));
        co_return !ec;
    }

  private:
    asio::experimental::concurrent_channel<void(asio::error_code)> channel_;
};

template <typename T>
class Mailbox {
  public:
    explicit Mailbox(const asio::any_io_executor& ex) : signal_(ex) {}

    // False once the receiver is gone.
    bool push(T value) {
        {
            std::lock_guard lock(mutex_);
            if (receiver_gone_) return false;
            queue_.push_back(std::move(value));
        }
        signal_.notify();
        return true;
    }

    // No more values will come; `pop` drains the rest, then yields `nullopt`.
    void close_senders() {
        {
            std::lock_guard lock(mutex_);
            senders_gone_ = true;
        }
        signal_.notify();
    }

    void close_receiver() {
        std::lock_guard lock(mutex_);
        receiver_gone_ = true;
        queue_.clear();
    }

    // `nullopt` once every sender is gone and the queue is empty, or when cancelled.
    // Cancel safe: a cancelled wait takes nothing.
    asio::awaitable<std::optional<T>> pop() {
        for (;;) {
            {
                std::lock_guard lock(mutex_);
                if (!queue_.empty()) {
                    T value = std::move(queue_.front());
                    queue_.pop_front();
                    co_return value;
                }
                if (senders_gone_) co_return std::nullopt;
            }
            if (!co_await signal_.wait()) co_return std::nullopt;
        }
    }

  private:
    std::mutex mutex_;
    std::deque<T> queue_;
    bool senders_gone_ = false;
    bool receiver_gone_ = false;
    Signal signal_;
};

// Cancels detached pumps when destroyed (posted to their executor, where they run).
class PumpGuard {
  public:
    explicit PumpGuard(asio::any_io_executor ex)
        : ex_(std::move(ex)), signal_(std::make_shared<asio::cancellation_signal>()) {}
    PumpGuard(PumpGuard&&) noexcept = default;
    PumpGuard& operator=(PumpGuard&&) noexcept = default;
    PumpGuard(const PumpGuard&) = delete;
    PumpGuard& operator=(const PumpGuard&) = delete;
    ~PumpGuard() {
        if (!signal_) return;
        asio::post(ex_, [signal = signal_] { signal->emit(asio::cancellation_type::terminal); });
    }

    // One signal drives one operation, so each pump gets its own guard.
    template <typename Awaitable>
    void spawn(Awaitable pump) {
        asio::co_spawn(ex_, std::move(pump),
                       asio::bind_cancellation_slot(signal_->slot(), [signal = signal_](std::exception_ptr) {}));
    }

  private:
    asio::any_io_executor ex_;
    std::shared_ptr<asio::cancellation_signal> signal_;
};

} // namespace loa::surfaces::proto::detail
