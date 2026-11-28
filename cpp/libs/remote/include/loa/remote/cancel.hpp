#pragma once

// A shared cancellation flag (Rust `CancellationToken`): copies share the state; blocking waits
// wake when it is cancelled.

#include <chrono>
#include <cstdint>
#include <functional>
#include <memory>

namespace loa::remote {

struct CancelState;

class CancelToken {
  public:
    CancelToken();

    // Sets the flag and runs the registered callbacks once. Thread-safe, idempotent.
    void cancel() const;
    [[nodiscard]] bool cancelled() const;
    // Blocks until cancelled or `timeout` passes; true when cancelled.
    bool wait_for(std::chrono::milliseconds timeout) const;

    // Keeps a callback registered until destroyed.
    class Callback {
      public:
        Callback() = default;
        Callback(Callback&& o) noexcept : state_(std::move(o.state_)), id_(o.id_) {}
        Callback& operator=(Callback&&) = delete;
        Callback(const Callback&) = delete;
        ~Callback();

      private:
        friend class CancelToken;
        Callback(std::shared_ptr<CancelState> state, std::uint64_t id) : state_(std::move(state)), id_(id) {}
        std::shared_ptr<CancelState> state_;
        std::uint64_t id_ = 0;
    };
    // Runs `fn` on cancellation (at once when already cancelled) while the guard lives.
    [[nodiscard]] Callback on_cancel(std::function<void()> fn) const;

  private:
    std::shared_ptr<CancelState> state_;
};

} // namespace loa::remote
