#include "loa/surfaces/feed.hpp"

#include <algorithm>

#include "loa/contracts/surface_channel.hpp"

namespace loa::surfaces {

FrameFeed::FrameFeed(const asio::any_io_executor& executor, std::size_t backlog)
    : tx_(std::make_unique<loa::contracts::surface::FrameSender>(executor, backlog)) {}

FrameFeed::~FrameFeed() = default;

std::shared_ptr<FrameReceiver> FrameFeed::subscribe() { return tx_->subscribe(); }

std::size_t FrameFeed::viewers() { return tx_->receiver_count(); }

void FrameFeed::publish(Frame frame) {
    frame.seq = seq_.fetch_add(1, std::memory_order_relaxed) + 1;
    auto shared = std::make_shared<const Frame>(std::move(frame));
    if (shared->keyframe) {
        std::lock_guard lock(mutex_);
        last_key_ = shared;
    }
    // No receivers is fine: nobody is watching right now.
    tx_->send(std::move(shared));
}

bool FrameFeed::republish_keyframe() {
    std::shared_ptr<const Frame> last;
    {
        std::lock_guard lock(mutex_);
        last = last_key_;
    }
    if (!last) return false;
    publish(*last);
    return true;
}

void InputLog::record(std::uint64_t seq, std::uint64_t sent_us) {
    highest_ = std::max(highest_, seq);
    if (entries_.size() == INPUT_HISTORY) entries_.pop_front();
    if (!entries_.empty()) sent_us = std::max(sent_us, entries_.back().first);
    entries_.emplace_back(sent_us, highest_);
}

std::uint64_t InputLog::seq_before(std::uint64_t captured_us) const {
    const auto it = std::ranges::partition_point(entries_, [&](const auto& e) { return e.first <= captured_us; });
    return it == entries_.begin() ? 0 : std::prev(it)->second;
}

} // namespace loa::surfaces
