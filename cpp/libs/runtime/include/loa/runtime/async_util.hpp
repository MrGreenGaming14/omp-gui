#pragma once

// Scheduling helpers on `std::chrono::steady_clock`; every call that reads the
// clock has an overload taking `now`, so tests drive time explicitly.

#include <algorithm>
#include <chrono>
#include <map>
#include <optional>
#include <vector>

namespace loa::runtime {

// Per-key deadlines: each `touch` pushes the key out by the window, so a burst
// of touches fires the key once, after it goes quiet.
template <typename K>
class KeyedDebouncer {
  public:
    using Clock = std::chrono::steady_clock;
    using Instant = Clock::time_point;

    explicit KeyedDebouncer(Clock::duration window) : window_(window) {}

    void touch(K key) { touch(std::move(key), Clock::now()); }
    void touch(K key, Instant now) { due_.insert_or_assign(std::move(key), now + window_); }

    // The earliest pending deadline.
    [[nodiscard]] std::optional<Instant> next() const {
        if (due_.empty()) return std::nullopt;
        return std::ranges::min_element(due_, {}, [](const auto& entry) { return entry.second; })->second;
    }

    [[nodiscard]] bool empty() const noexcept { return due_.empty(); }

    // Removes and returns, in key order, the keys due by `now` (every key when `all`).
    std::vector<K> take_due(Instant now, bool all) {
        std::vector<K> ready;
        for (auto it = due_.begin(); it != due_.end();) {
            if (all || it->second <= now) {
                ready.push_back(it->first);
                it = due_.erase(it);
            } else {
                ++it;
            }
        }
        return ready;
    }

  private:
    Clock::duration window_;
    std::map<K, Instant> due_;
};

} // namespace loa::runtime
