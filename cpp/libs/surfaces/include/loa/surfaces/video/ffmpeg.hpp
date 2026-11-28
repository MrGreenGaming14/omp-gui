#pragma once

// Low-latency ffmpeg hardware encoders, selected by actually coding pictures.
// Raw Annex B output is split at inserted AUDs, or after a short quiet period
// so a picture needn't wait for the next one. Port of
// `loa-surfaces/src/proto/video/ffmpeg.rs`.

#include <array>
#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/surfaces/video/encode.hpp"

namespace loa::surfaces::video {

inline constexpr std::string_view NAME_PREFIX = "ffmpeg ";

struct KnownEncoder {
    std::string_view name;
    Codec codec;
    Accel accel;
};

// Probe order.
inline constexpr std::array<KnownEncoder, 8> KNOWN{{
    {"h264_vaapi", Codec::H264, Accel::Vaapi},
    {"hevc_vaapi", Codec::Hevc, Accel::Vaapi},
    {"h264_nvenc", Codec::H264, Accel::Nvenc},
    {"hevc_nvenc", Codec::Hevc, Accel::Nvenc},
    {"h264_qsv", Codec::H264, Accel::Qsv},
    {"hevc_qsv", Codec::Hevc, Accel::Qsv},
    {"h264_v4l2m2m", Codec::H264, Accel::V4l2M2m},
    {"hevc_v4l2m2m", Codec::Hevc, Accel::V4l2M2m},
}};

struct FfmpegConfig {
    std::filesystem::path binary;

    // `ffmpeg` on `$PATH`.
    [[nodiscard]] static std::optional<FfmpegConfig> discover();
};

// Codes two pictures with every encoder in parallel: drivers and devices may
// be absent even though ffmpeg advertises the encoder.
[[nodiscard]] std::vector<ProbeResult> probe(const FfmpegConfig& config);

// The arguments `start` runs ffmpeg with; `vaapi_device` is the render node
// for VA-API encoders.
[[nodiscard]] std::vector<std::string> start_args(std::string_view name, Codec codec, Accel accel, std::uint32_t width,
                                                  std::uint32_t height, const std::optional<std::filesystem::path>& vaapi_device);

// Beginning of the second picture's AUD, if already present in the buffer.
[[nodiscard]] std::optional<std::size_t> next_access_unit(Codec codec, std::span<const std::uint8_t> data);

class FfmpegEncoder final : public VideoEncoder {
  public:
    [[nodiscard]] static Result<std::unique_ptr<FfmpegEncoder>> spawn(const FfmpegConfig& config, std::string_view name,
                                                                      std::pair<std::uint32_t, std::uint32_t> size);
    ~FfmpegEncoder() override;
    FfmpegEncoder(const FfmpegEncoder&) = delete;
    FfmpegEncoder& operator=(const FfmpegEncoder&) = delete;

    [[nodiscard]] Result<AccessUnit> encode(std::span<const std::uint8_t> rgba, bool keyframe) override;

    struct Process;

  private:
    FfmpegEncoder() = default;
    [[nodiscard]] ProtoError error(std::string_view what) const;
    [[nodiscard]] Result<std::vector<std::uint8_t>> read_picture();

    std::filesystem::path binary_;
    std::string_view name_;
    Codec codec_ = Codec::H264;
    Accel accel_ = Accel::Software;
    std::pair<std::uint32_t, std::uint32_t> size_;
    std::unique_ptr<Process> process_;
    std::vector<std::uint8_t> pending_;
    std::uint64_t frames_ = 0;
};

} // namespace loa::surfaces::video
