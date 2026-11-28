#pragma once

// Live video: `VideoSurface` codes a surface's pictures as H.264/HEVC with a
// probed hardware encoder (software H.264 bridging), or passes them on. Port of
// `loa-surfaces/src/proto/video/mod.rs`.

#include <memory>
#include <optional>
#include <string_view>
#include <thread>
#include <vector>

#include "loa/contracts/surface.hpp"
#include "loa/surfaces/video/encode.hpp"
#include "loa/surfaces/video/ffmpeg.hpp"

namespace loa::contracts::surface {
class FrameSender;
}

namespace loa::surfaces::video {

namespace cs = contracts::surface;

// The encoders this machine has, each tried once for real.
class LiveVideo {
  public:
    // Tries every known ffmpeg hardware encoder (blocking).
    [[nodiscard]] static LiveVideo probe(std::optional<FfmpegConfig> ffmpeg);
    [[nodiscard]] static LiveVideo with_probed(std::optional<FfmpegConfig> ffmpeg, std::vector<ProbeResult> probed);

    [[nodiscard]] const std::vector<ProbeResult>& encoders() const noexcept { return probed_; }
    // The first `accepted` codec with working hardware, else software H.264
    // if accepted, else `std::nullopt` (source frames pass on).
    [[nodiscard]] std::optional<EncoderInfo> choose(std::span<const Encoding> accepted) const;
    [[nodiscard]] Result<LiveEncoder> open(const EncoderInfo& info, std::pair<std::uint32_t, std::uint32_t> size) const;

  private:
    std::optional<FfmpegConfig> ffmpeg_;
    std::vector<ProbeResult> probed_;
};

[[nodiscard]] cs::StreamInfo stream_info(const EncoderInfo& info);

// Encoder name of frames passed on as the source made them.
inline constexpr std::string_view SOURCE = "source";

// Frames are rebuilt and coded by a worker thread; the rest goes to `inner`.
class VideoSurface final : public cs::Surface {
  public:
    // Passes source frames on until a viewer negotiates video while `enabled`.
    // Keyframe requests to the source run on `executor`.
    [[nodiscard]] static cs::Result<std::shared_ptr<VideoSurface>> make(std::shared_ptr<cs::Surface> inner,
                                                                        std::shared_ptr<const LiveVideo> live, bool enabled,
                                                                        const asio::any_io_executor& executor);
    ~VideoSurface() override;
    VideoSurface(const VideoSurface&) = delete;
    VideoSurface& operator=(const VideoSurface&) = delete;

    // Turns video on or off, redoing the last negotiation.
    [[nodiscard]] contracts::Awaitable<cs::Result<std::optional<cs::StreamInfo>>> set_video(bool enabled);

    [[nodiscard]] cs::SurfaceInfo info() const override;
    // A new subscriber gets the current picture without asking: the source is asked for a keyframe.
    [[nodiscard]] cs::Result<std::shared_ptr<cs::FrameReceiver>> frames() override;
    [[nodiscard]] contracts::Awaitable<cs::Result<void>> request_keyframe() override;
    // A resize warms the hardware encoder for the new size up right away.
    [[nodiscard]] contracts::Awaitable<cs::Result<void>> input(cs::InputEvent event) override;
    [[nodiscard]] contracts::Awaitable<cs::Result<cs::Document>> document() override;
    [[nodiscard]] contracts::Awaitable<cs::Result<cs::Bytes>> screenshot() override;
    [[nodiscard]] contracts::Awaitable<cs::Result<std::optional<cs::StreamInfo>>> negotiate(std::vector<Encoding> accepted) override;
    [[nodiscard]] std::optional<cs::StreamInfo> stream() const override;

    struct Shared;

  private:
    VideoSurface() = default;

    std::shared_ptr<cs::Surface> inner_;
    std::shared_ptr<Shared> shared_;
    std::shared_ptr<cs::FrameSender> frames_;
    std::thread worker_;
};

} // namespace loa::surfaces::video
