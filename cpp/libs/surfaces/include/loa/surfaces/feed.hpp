#pragma once

// Frame fan-out (`FrameFeed`) and the input clock (`InputLog`) that stamps
// `Frame::input_seq`, shared by every pixel surface. Port of
// `loa-surfaces/src/feed.rs`.

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <deque>
#include <memory>
#include <mutex>
#include <utility>

#include "loa/contracts/async.hpp"
#include "loa/contracts/surface.hpp"

namespace loa::contracts::surface {
class FrameSender;
}

namespace loa::surfaces {

using loa::contracts::surface::Frame;
using loa::contracts::surface::FrameReceiver;

// How browser tabs are captured; the surface launcher shows the same choice.
enum class CaptureMode : std::uint8_t {
    // Headless Chrome's JPEG screencast.
    Screencast,
    // Headed Chrome on an Xvfb virtual display per tab, captured raw.
    VirtualDisplay,
};

// Inputs remembered for stamping frames captured a little after them.
inline constexpr std::size_t INPUT_HISTORY = 256;

// Numbers frames, keeps the last keyframe for late viewers and fans frames out
// to every viewer of one surface. A viewer more than `backlog` frames behind
// loses the oldest ones (tokio broadcast's `Lagged`).
class FrameFeed {
  public:
    FrameFeed(const asio::any_io_executor& executor, std::size_t backlog);
    ~FrameFeed();
    FrameFeed(const FrameFeed&) = delete;
    FrameFeed& operator=(const FrameFeed&) = delete;

    [[nodiscard]] std::shared_ptr<FrameReceiver> subscribe();
    // Viewers subscribed right now.
    [[nodiscard]] std::size_t viewers();
    // Numbers `frame` (its `seq` is replaced) and sends it to every viewer.
    void publish(Frame frame);
    // Sends the last keyframe again under a new sequence number. Returns false
    // when no keyframe was captured yet.
    bool republish_keyframe();

  private:
    std::unique_ptr<loa::contracts::surface::FrameSender> tx_;
    std::atomic<std::uint64_t> seq_{0};
    std::mutex mutex_;
    std::shared_ptr<const Frame> last_key_;
};

// Remembers when inputs were dispatched, to find the newest input sent before
// a frame was captured.
class InputLog {
  public:
    void record(std::uint64_t seq, std::uint64_t sent_us);
    // Highest seq dispatched at or before `captured_us`, 0 when none is known.
    [[nodiscard]] std::uint64_t seq_before(std::uint64_t captured_us) const;

  private:
    // `(sent_us, highest seq dispatched so far)`, in dispatch order.
    std::deque<std::pair<std::uint64_t, std::uint64_t>> entries_;
    std::uint64_t highest_ = 0;
};

} // namespace loa::surfaces
