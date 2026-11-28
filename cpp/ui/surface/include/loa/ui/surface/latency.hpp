#pragma once

// Input to pixels latency on the viewer's clock: each input's send time is paired with the first
// shown frame whose `input_seq` covers it, over a rolling window of samples. Port of
// `crates/loa-ui-surface/src/latency.rs`.

#include <cstddef>
#include <cstdint>
#include <deque>
#include <optional>
#include <utility>
#include <vector>

namespace loa::ui::surface {

// The input to pixels budget. Past it the meter warns.
inline constexpr std::uint64_t BUDGET_US = 50'000;
// Samples kept in the rolling window.
inline constexpr std::size_t WINDOW = 100;

// What the tab shows.
struct Reading {
    std::uint64_t median_us = 0;
    std::uint64_t p95_us = 0;
    // Samples behind the numbers.
    std::size_t samples = 0;
    // The 95th percentile is past `BUDGET_US`.
    bool warning = false;
    friend bool operator==(const Reading&, const Reading&) = default;
};

// The measurement core. Time comes in as arguments, so it is pure.
class LatencyMeter {
  public:
    // A meter keeping the last `window` samples (at least one).
    explicit LatencyMeter(std::size_t window = WINDOW);

    // Records input `seq` sent at `sent_us`. Sequence numbers go up.
    void input_sent(std::uint64_t seq, std::uint64_t sent_us);
    // Records a frame shown at `now_us` that includes every input up to `input_seq`. Each
    // covered input yields one sample.
    void frame_shown(std::uint64_t input_seq, std::uint64_t now_us);
    // The current median and p95, `nullopt` before the first sample.
    [[nodiscard]] std::optional<Reading> reading() const { return reading_; }

  private:
    // `(seq, sent_us)` in send order.
    std::deque<std::pair<std::uint64_t, std::uint64_t>> pending_;
    std::deque<std::uint64_t> samples_;
    std::size_t window_;
    // Scratch buffer for the percentiles, reused across frames.
    std::vector<std::uint64_t> sorted_;
    std::optional<Reading> reading_;
};

} // namespace loa::ui::surface
