#pragma once

// Per subscriber bounded queues.
//
// The writer pushes into each queue without waiting: a full queue marks its
// subscriber lagged and detaches it, so one slow reader never holds up the
// writer or the other readers.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <variant>

#include "loa/core/dom.hpp"
#include "loa/core/patch.hpp"
#include "loa/runtime/executor.hpp"

namespace loa::bus {

// How a subscriber is fed.
struct Policy {
    // Undelivered events the subscriber may hold. One more marks it lagged.
    // Values below 1 count as 1.
    std::size_t capacity = 1;
    // Merge consecutive `append` patches to the same node while they wait in
    // the queue. For views that only need the latest text (the UI); never for
    // subscribers that need every patch (peers, the journal).
    bool coalesce_appends = false;

    // Every patch, one by one.
    [[nodiscard]] static constexpr Policy every_patch(std::size_t capacity) { return {capacity, false}; }
    // Streaming appends to one node merged while the subscriber is behind.
    [[nodiscard]] static constexpr Policy coalescing(std::size_t capacity) { return {capacity, true}; }

    friend bool operator==(const Policy&, const Policy&) = default;
};

// Consecutive `append` patches to one node, merged for a coalescing
// subscriber. Covers sequence numbers `first_seq..=last_seq`.
struct Appends {
    core::NodeId node;
    std::uint64_t first_seq = 0;
    std::uint64_t last_seq = 0;
    // Timestamp of the last merged patch.
    std::int64_t ts = 0;
    // The merged text, in order.
    std::string text;

    // Applies the merged appends to a DOM mirror, which ends at `last_seq`.
    // Consumes the text.
    std::expected<void, core::ApplyError> apply_to(core::Dom& dom) &&;

    friend bool operator==(const Appends&, const Appends&) = default;
};

namespace event {
struct Patch {
    std::shared_ptr<const core::Patch> patch;
    // Compares the patches, like Rust's `Arc<Patch>` equality.
    friend bool operator==(const Patch& a, const Patch& b) {
        return a.patch == b.patch || (a.patch && b.patch && *a.patch == *b.patch);
    }
};
// The subscriber fell behind and was detached. Everything before
// `resync_from` was delivered; it resubscribes or replays the journal from
// `resync_from`. Nothing follows this event.
struct Lagged {
    std::uint64_t resync_from = 0;
    friend bool operator==(const Lagged&, const Lagged&) = default;
};
} // namespace event

// What a subscriber receives. `Appends` only reaches subscribers with
// `Policy::coalesce_appends`.
using Event = std::variant<event::Patch, Appends, event::Lagged>;

// Applies a patch or merged appends to a DOM mirror. Returns `false` for
// `Lagged`, which carries nothing to apply.
std::expected<bool, core::ApplyError> apply_to(Event&& event, core::Dom& dom);

enum class TryRecvError : std::uint8_t {
    Empty,
    // The session closed, or the subscriber lagged and got its notice.
    Closed,
};

[[nodiscard]] std::string_view to_string(TryRecvError e) noexcept;

class Subscription;

namespace detail {
// One subscriber's queue; defined in `src/writer.cpp`.
class Queue;
[[nodiscard]] std::shared_ptr<Queue> make_queue(Policy policy);
// Offers `patch`. Never waits. Returns `false` once the queue no longer takes
// patches (lagged, closed or receiver dropped).
bool push(Queue& queue, const std::shared_ptr<const core::Patch>& patch);
// Ends the subscription after what is already queued.
void close(Queue& queue);
[[nodiscard]] Subscription subscription(std::shared_ptr<Queue> queue);
} // namespace detail

// A subscriber to one session. `try_recv` works from any thread; `recv` is an
// Asio coroutine. Destroying it detaches the subscriber. Not shared between
// threads: one `recv` at a time.
class Subscription {
  public:
    Subscription(Subscription&&) noexcept = default;
    Subscription& operator=(Subscription&&) noexcept;
    Subscription(const Subscription&) = delete;
    Subscription& operator=(const Subscription&) = delete;
    ~Subscription();

    // The next event without waiting.
    [[nodiscard]] std::expected<Event, TryRecvError> try_recv();

    // Waits for the next event. `nullopt` once the session closed and
    // everything queued was delivered, or after `Lagged`.
    [[nodiscard]] asio::awaitable<std::optional<Event>, asio::any_io_executor> recv();

    // Events waiting in the queue.
    [[nodiscard]] std::size_t pending() const;

  private:
    friend Subscription detail::subscription(std::shared_ptr<detail::Queue> queue);
    explicit Subscription(std::shared_ptr<detail::Queue> queue) : queue_(std::move(queue)) {}
    void detach() noexcept;

    std::shared_ptr<detail::Queue> queue_;
};

} // namespace loa::bus
