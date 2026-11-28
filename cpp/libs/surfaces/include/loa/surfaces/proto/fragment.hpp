#pragma once

// Frames split into datagram-sized fragments and reassembled on the viewer
// (port of `loa-surfaces/src/proto/fragment.rs`). Nothing is resent: incomplete
// or older frames are dropped once a newer one completes, and a video loss
// skips to the next keyframe, asking for one.

#include <cstddef>
#include <cstdint>
#include <deque>
#include <functional>
#include <optional>
#include <span>
#include <vector>

#include "loa/contracts/surface.hpp"
#include "loa/surfaces/proto/error.hpp"
#include "loa/surfaces/proto/wire.hpp"

namespace loa::surfaces::proto {

// Most fragments one frame may have, about 75 MiB at a 1200 byte limit.
inline constexpr std::uint32_t MAX_FRAGMENTS = 65'536;
// Frames being reassembled at once; the oldest partial frame is evicted for a newer one.
inline constexpr std::size_t MAX_PENDING = 8;

// Hands `frame` to `emit` as encoded datagrams of at most `max_datagram` bytes;
// stops at the first error `emit` returns.
[[nodiscard]] Result<void> fragment(const contracts::surface::Frame& frame, std::size_t max_datagram,
                                    const std::function<Result<void>(Bytes)>& emit);

// What the viewer does after one datagram or stream frame.
struct Pushed {
    std::optional<contracts::surface::Frame> frame;
    bool request_keyframe = false;
    friend bool operator==(const Pushed&, const Pushed&) = default;
};

class Reassembler {
  public:
    // The server sends a keyframe on open, so a fresh viewer waits without asking.
    Reassembler() = default;

    // Malformed and unknown datagrams count as lost.
    Pushed push_datagram(std::span<const std::uint8_t> datagram);
    Pushed push_frame(contracts::surface::Frame frame);

  private:
    struct Partial {
        std::uint64_t seq = 0;
        std::optional<FrameHeader> header;
        std::vector<std::optional<Bytes>> parts;
        std::uint32_t have = 0;
    };

    Pushed push_fragment(Fragment fragment);
    [[nodiscard]] bool is_stale(std::uint64_t seq) const noexcept { return last_ && seq <= *last_; }

    // Seq of the newest frame delivered or skipped.
    std::optional<std::uint64_t> last_;
    // Partial frames, sorted by seq.
    std::deque<Partial> pending_;
    bool awaiting_keyframe_ = true;
};

} // namespace loa::surfaces::proto
