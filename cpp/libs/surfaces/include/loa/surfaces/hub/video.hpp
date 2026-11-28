#pragma once

// Live video for viewers across the network (CMake option
// `LOA_SURFACES_REMOTE_VIDEO`, the Rust `remote-video` feature): browser tabs
// and VMs are coded as H.264/HEVC for peers that decode it. Port of
// `loa-surfaces/src/hub/video.rs`.

#include <atomic>
#include <memory>
#include <mutex>
#include <optional>
#include <utility>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/surface.hpp"
#include "loa/surfaces/hub/error.hpp"
#include "loa/surfaces/video/live.hpp"

namespace loa::surfaces::hub {

struct Opened;

// What remote video starts with.
struct VideoConfig {
    // Live surfaces stream video to network viewers that decode it.
    bool enabled = true;
    // The ffmpeg hardware encoders run through.
    std::optional<video::FfmpegConfig> ffmpeg;
    // This machine's encoders; none tries them on first need.
    std::shared_ptr<const video::LiveVideo> encoders;

    // On, with ffmpeg from `$PATH`.
    [[nodiscard]] static VideoConfig discover();
};

// The hub's video encoders and the surfaces it codes.
class RemoteVideo {
  public:
    explicit RemoteVideo(const VideoConfig& config);

    // This machine's encoders, tried on the first call (it starts ffmpeg a few times, so it
    // takes a moment; the probe runs on its own thread).
    [[nodiscard]] loa::contracts::Awaitable<std::shared_ptr<const video::LiveVideo>> encoders();

    [[nodiscard]] bool enabled() const noexcept { return enabled_.load(std::memory_order_acquire); }
    // Turns video on or off, for surfaces already coded too.
    [[nodiscard]] loa::contracts::Awaitable<void> set_enabled(bool enabled);

    // `opened` for a viewer across the network: live pixel surfaces (browser tabs, VMs) are coded
    // as video, one encoder per surface; the rest go as they are.
    [[nodiscard]] loa::contracts::Awaitable<Result<std::shared_ptr<cs::Surface>>> surface(Opened opened);
    // Stops coding `uri`, or every surface for none.
    void forget(const cs::SurfaceUri* uri);

  private:
    [[nodiscard]] std::shared_ptr<video::VideoSurface> coded(const cs::SurfaceUri& uri) const;

    std::optional<video::FfmpegConfig> ffmpeg_;
    mutable std::mutex mutex_;
    std::shared_ptr<const video::LiveVideo> encoders_;
    std::atomic<bool> enabled_;
    std::vector<std::pair<cs::SurfaceUri, std::shared_ptr<video::VideoSurface>>> videos_;
};

} // namespace loa::surfaces::hub
