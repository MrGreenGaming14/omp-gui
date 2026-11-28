#include "loa/ui/surface/latency.hpp"

#include <algorithm>

namespace loa::ui::surface {

namespace {

// Inputs waiting for a frame. Older ones are forgotten past this.
constexpr std::size_t MAX_PENDING = 4096;

// Nearest rank percentile of a non-empty ascending range.
std::uint64_t percentile(const std::vector<std::uint64_t>& sorted, std::size_t percent) {
    const std::size_t rank = std::max<std::size_t>((sorted.size() * percent + 99) / 100, 1);
    return sorted[rank - 1];
}

} // namespace

LatencyMeter::LatencyMeter(std::size_t window) : window_(std::max<std::size_t>(window, 1)) { sorted_.reserve(window_); }

void LatencyMeter::input_sent(std::uint64_t seq, std::uint64_t sent_us) {
    if (pending_.size() == MAX_PENDING) pending_.pop_front();
    pending_.emplace_back(seq, sent_us);
}

void LatencyMeter::frame_shown(std::uint64_t input_seq, std::uint64_t now_us) {
    const std::size_t before = pending_.size();
    while (!pending_.empty() && pending_.front().first <= input_seq) {
        const std::uint64_t sent_us = pending_.front().second;
        pending_.pop_front();
        if (samples_.size() == window_) samples_.pop_front();
        samples_.push_back(now_us > sent_us ? now_us - sent_us : 0);
    }
    if (pending_.size() == before) return;
    sorted_.assign(samples_.begin(), samples_.end());
    std::ranges::sort(sorted_);
    const std::uint64_t median = percentile(sorted_, 50);
    const std::uint64_t p95 = percentile(sorted_, 95);
    reading_ = Reading{median, p95, sorted_.size(), p95 > BUDGET_US};
}

} // namespace loa::ui::surface
