#pragma once

// Feeds read until they end: broadcast, watch and unbounded channels, and an
// optional source that ends at once (port of `loa_runtime::feed`). The Rust
// sources are tokio's channel receivers; here the channels are in-house and
// their receivers are the sources:
// - `BroadcastReceiver<T>` yields `T`, or `std::unexpected(skipped)` after it
//   fell `skipped` items behind (tokio `RecvError::Lagged`);
// - `WatchReceiver<T>` yields once per change of the watched value;
// - `UnboundedReceiver<T>` yields each item sent.
// Each ends once every sender is gone and nothing is left to read.

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <deque>
#include <expected>
#include <memory>
#include <mutex>
#include <optional>
#include <utility>
#include <variant>
#include <vector>

#include <asio/any_io_executor.hpp>
#include <asio/awaitable.hpp>

#include "loa/app_runtime/source.hpp"

namespace loa::app_runtime {

// A wake-up for one waiting coroutine. A `notify` without a waiter is kept
// until the next `wait`, so no wake-up is lost between a check and the wait.
class Signal {
  public:
    Signal();
    ~Signal();
    Signal(const Signal&) = delete;
    Signal& operator=(const Signal&) = delete;

    // Thread-safe.
    void notify();
    // Resumes after the next (or a pending) `notify`. One waiter at a time.
    [[nodiscard]] contracts::Awaitable<void> wait();

  private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

namespace detail {

// Receiver wake-ups registered with a channel.
class Waiters {
  public:
    void add(const std::shared_ptr<Signal>& signal) { list_.push_back(signal); }
    // Wakes every live receiver and forgets the dropped ones.
    void notify_all() {
        std::erase_if(list_, [](const std::weak_ptr<Signal>& weak) {
            auto signal = weak.lock();
            if (signal) signal->notify();
            return !signal;
        });
    }
    [[nodiscard]] std::size_t live() const {
        std::size_t n = 0;
        for (const auto& weak : list_) n += weak.expired() ? 0 : 1;
        return n;
    }

  private:
    std::vector<std::weak_ptr<Signal>> list_;
};

// Counts the senders of a channel core; the last one gone closes it.
template <typename Core>
class SenderHandle {
  public:
    explicit SenderHandle(std::shared_ptr<Core> core) : core_(std::move(core)) {
        std::scoped_lock lock(core_->mutex);
        ++core_->senders;
    }
    SenderHandle(const SenderHandle& other) : SenderHandle(other.core_) {}
    SenderHandle(SenderHandle&& other) noexcept = default;
    SenderHandle& operator=(SenderHandle other) noexcept {
        std::swap(core_, other.core_);
        return *this;
    }
    ~SenderHandle() {
        if (!core_) return;
        std::scoped_lock lock(core_->mutex);
        if (--core_->senders == 0) core_->waiters.notify_all();
    }

  protected:
    std::shared_ptr<Core> core_;
};

template <typename T>
struct BroadcastCore {
    std::mutex mutex;
    std::size_t capacity = 1;
    std::deque<T> items;
    // Sequence of `items.front()`; `tail` is the next sequence sent.
    std::uint64_t head = 0;
    std::uint64_t tail = 0;
    std::size_t senders = 0;
    Waiters waiters;
};

template <typename T>
struct WatchCore {
    std::mutex mutex;
    T value;
    std::uint64_t version = 0;
    std::size_t senders = 0;
    Waiters waiters;
};

template <typename T>
struct UnboundedCore {
    std::mutex mutex;
    std::deque<T> items;
    std::size_t senders = 0;
    bool receiver_gone = false;
    Waiters waiters;
};

} // namespace detail

// ---- broadcast -------------------------------------------------------------

template <typename T>
class BroadcastReceiver;

// Sends to every receiver; each keeps the last `capacity` items it hasn't read.
template <typename T>
class BroadcastSender : public detail::SenderHandle<detail::BroadcastCore<T>> {
    using Base = detail::SenderHandle<detail::BroadcastCore<T>>;

  public:
    using Base::Base;

    // Sends `value`; returns how many receivers can still read it.
    std::size_t send(T value) const {
        auto& core = *this->core_;
        std::scoped_lock lock(core.mutex);
        core.items.push_back(std::move(value));
        ++core.tail;
        while (core.items.size() > core.capacity) {
            core.items.pop_front();
            ++core.head;
        }
        core.waiters.notify_all();
        return core.waiters.live();
    }

    // A receiver of what is sent from now on.
    [[nodiscard]] BroadcastReceiver<T> subscribe() const { return BroadcastReceiver<T>(this->core_); }
};

// One reader of a broadcast channel: `T`, or the count it skipped after falling behind.
template <typename T>
class BroadcastReceiver final : public Source<std::expected<T, std::uint64_t>> {
  public:
    explicit BroadcastReceiver(std::shared_ptr<detail::BroadcastCore<T>> core)
        : core_(std::move(core)), signal_(std::make_shared<Signal>()) {
        std::scoped_lock lock(core_->mutex);
        next_ = core_->tail;
        core_->waiters.add(signal_);
    }

    // The next item without waiting: `nullopt` when none is ready, else the
    // item or the skipped count. `closed` is set once nothing will follow.
    [[nodiscard]] std::optional<std::expected<T, std::uint64_t>> try_recv(bool* closed = nullptr) {
        std::scoped_lock lock(core_->mutex);
        if (closed) *closed = false;
        if (next_ < core_->head) {
            const std::uint64_t skipped = core_->head - next_;
            next_ = core_->head;
            return std::expected<T, std::uint64_t>(std::unexpect, skipped);
        }
        if (next_ < core_->tail) {
            T item = core_->items[static_cast<std::size_t>(next_ - core_->head)];
            ++next_;
            return std::expected<T, std::uint64_t>(std::move(item));
        }
        if (closed) *closed = core_->senders == 0;
        return std::nullopt;
    }

    contracts::Awaitable<std::optional<std::expected<T, std::uint64_t>>> next() override {
        for (;;) {
            bool closed = false;
            if (auto item = try_recv(&closed)) co_return item;
            if (closed) co_return std::nullopt;
            co_await signal_->wait();
        }
    }

  private:
    std::shared_ptr<detail::BroadcastCore<T>> core_;
    std::shared_ptr<Signal> signal_;
    std::uint64_t next_ = 0;
};

// A broadcast channel holding `capacity` (at least 1) unread items per receiver.
template <typename T>
[[nodiscard]] std::pair<BroadcastSender<T>, BroadcastReceiver<T>> broadcast(std::size_t capacity) {
    auto core = std::make_shared<detail::BroadcastCore<T>>();
    core->capacity = capacity < 1 ? 1 : capacity;
    BroadcastSender<T> sender(core);
    BroadcastReceiver<T> receiver(core);
    return {std::move(sender), std::move(receiver)};
}

// ---- watch -----------------------------------------------------------------

template <typename T>
class WatchReceiver;

// Holds one value; receivers learn of each change.
template <typename T>
class WatchSender : public detail::SenderHandle<detail::WatchCore<T>> {
    using Base = detail::SenderHandle<detail::WatchCore<T>>;

  public:
    using Base::Base;

    void send(T value) const {
        auto& core = *this->core_;
        std::scoped_lock lock(core.mutex);
        core.value = std::move(value);
        ++core.version;
        core.waiters.notify_all();
    }

    [[nodiscard]] T borrow() const {
        std::scoped_lock lock(this->core_->mutex);
        return this->core_->value;
    }

    // A receiver that has seen the current value.
    [[nodiscard]] WatchReceiver<T> subscribe() const { return WatchReceiver<T>(this->core_); }
};

// Each change of the watched value, as `std::monostate` (Rust `()`).
template <typename T>
class WatchReceiver final : public Source<std::monostate> {
  public:
    explicit WatchReceiver(std::shared_ptr<detail::WatchCore<T>> core)
        : core_(std::move(core)), signal_(std::make_shared<Signal>()) {
        std::scoped_lock lock(core_->mutex);
        seen_ = core_->version;
        core_->waiters.add(signal_);
    }

    // The current value, marking it seen.
    [[nodiscard]] T borrow_and_update() {
        std::scoped_lock lock(core_->mutex);
        seen_ = core_->version;
        return core_->value;
    }

    [[nodiscard]] T borrow() const {
        std::scoped_lock lock(core_->mutex);
        return core_->value;
    }

    // Waits for a value not seen yet; `false` once the sender is gone.
    contracts::Awaitable<bool> changed() {
        for (;;) {
            {
                std::scoped_lock lock(core_->mutex);
                if (core_->version != seen_) {
                    seen_ = core_->version;
                    co_return true;
                }
                if (core_->senders == 0) co_return false;
            }
            co_await signal_->wait();
        }
    }

    contracts::Awaitable<std::optional<std::monostate>> next() override {
        if (co_await changed()) co_return std::monostate{};
        co_return std::nullopt;
    }

  private:
    std::shared_ptr<detail::WatchCore<T>> core_;
    std::shared_ptr<Signal> signal_;
    std::uint64_t seen_ = 0;
};

template <typename T>
[[nodiscard]] std::pair<WatchSender<T>, WatchReceiver<T>> watch(T initial) {
    auto core = std::make_shared<detail::WatchCore<T>>();
    core->value = std::move(initial);
    WatchSender<T> sender(core);
    WatchReceiver<T> receiver(core);
    return {std::move(sender), std::move(receiver)};
}

// ---- unbounded -------------------------------------------------------------

// Queues items for the single receiver without bound.
template <typename T>
class UnboundedSender : public detail::SenderHandle<detail::UnboundedCore<T>> {
    using Base = detail::SenderHandle<detail::UnboundedCore<T>>;

  public:
    using Base::Base;

    // `false` once the receiver is gone; the item is dropped.
    bool send(T value) const {
        auto& core = *this->core_;
        std::scoped_lock lock(core.mutex);
        if (core.receiver_gone) return false;
        core.items.push_back(std::move(value));
        core.waiters.notify_all();
        return true;
    }
};

template <typename T>
class UnboundedReceiver final : public Source<T> {
  public:
    explicit UnboundedReceiver(std::shared_ptr<detail::UnboundedCore<T>> core)
        : core_(std::move(core)), signal_(std::make_shared<Signal>()) {
        std::scoped_lock lock(core_->mutex);
        core_->waiters.add(signal_);
    }
    UnboundedReceiver(UnboundedReceiver&&) noexcept = default;
    UnboundedReceiver& operator=(UnboundedReceiver&&) noexcept = default;
    ~UnboundedReceiver() override {
        if (!core_) return;
        std::scoped_lock lock(core_->mutex);
        core_->receiver_gone = true;
        core_->items.clear();
    }

    // The next item; `nullopt` once every sender is gone and the queue is empty.
    contracts::Awaitable<std::optional<T>> recv() {
        for (;;) {
            {
                std::scoped_lock lock(core_->mutex);
                if (!core_->items.empty()) {
                    T item = std::move(core_->items.front());
                    core_->items.pop_front();
                    co_return item;
                }
                if (core_->senders == 0) co_return std::nullopt;
            }
            co_await signal_->wait();
        }
    }

    contracts::Awaitable<std::optional<T>> next() override { co_return co_await recv(); }

  private:
    std::shared_ptr<detail::UnboundedCore<T>> core_;
    std::shared_ptr<Signal> signal_;
};

template <typename T>
[[nodiscard]] std::pair<UnboundedSender<T>, UnboundedReceiver<T>> unbounded() {
    auto core = std::make_shared<detail::UnboundedCore<T>>();
    UnboundedSender<T> sender(core);
    UnboundedReceiver<T> receiver(core);
    return {std::move(sender), std::move(receiver)};
}

// ---- optional --------------------------------------------------------------

// A source that may be absent: no source ends at once (Rust `Option<S>`).
template <typename T>
class OptionalSource final : public Source<T> {
  public:
    OptionalSource() = default;
    explicit OptionalSource(std::unique_ptr<Source<T>> source) : source_(std::move(source)) {}

    contracts::Awaitable<std::optional<T>> next() override {
        if (!source_) co_return std::nullopt;
        co_return co_await source_->next();
    }

  private:
    std::unique_ptr<Source<T>> source_;
};

} // namespace loa::app_runtime
