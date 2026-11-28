#pragma once

// RGBA pictures to H.264/HEVC access units: the `VideoEncoder` port, the
// software H.264 encoder (openh264) and `LiveEncoder`. Port of
// `loa-surfaces/src/proto/video/encode.rs`.

#include <cstddef>
#include <cstdint>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/surface.hpp"
#include "loa/surfaces/proto/error.hpp"

namespace loa::surfaces::video {

using contracts::surface::Bytes;
using contracts::surface::Encoding;
using contracts::surface::Frame;
using proto::ProtoError;
using proto::Result;

// `ProtoError::Video`.
[[nodiscard]] ProtoError video_error(std::string message);

inline constexpr std::uint32_t FPS = 60;
// Frames between periodic keyframes; requested keyframes come on top.
inline constexpr std::uint32_t KEYFRAME_INTERVAL = 600;
// Constant quantizer, 0 (best) to 51.
inline constexpr std::uint8_t QP = 26;

enum class Codec : std::uint8_t { H264, Hevc };

[[nodiscard]] Encoding encoding(Codec codec) noexcept;

// What a codec's NAL unit types mean.
struct Nal {
    // The codec's name in ffmpeg.
    std::string_view name;
    // Right shift and mask of the type in a NAL unit's first byte.
    std::uint8_t shift;
    std::uint8_t mask;
    // Access unit delimiter.
    std::uint8_t aud;
    // IDR (H.264) or IRAP (HEVC) slices, inclusive.
    std::uint8_t keyframe_first, keyframe_last;
    std::uint8_t picture_first, picture_last;
    std::span<const std::uint8_t> parameter_sets;

    [[nodiscard]] std::uint8_t type_of(std::uint8_t header) const noexcept { return (header >> shift) & mask; }
};

[[nodiscard]] const Nal& nal(Codec codec) noexcept;

enum class Accel : std::uint8_t { Vaapi, Nvenc, Qsv, V4l2M2m, Software };

[[nodiscard]] constexpr bool is_hardware(Accel accel) noexcept { return accel != Accel::Software; }

// e.g. `ffmpeg h264_vaapi` or `rusty_h264`.
struct EncoderInfo {
    std::string name;
    Codec codec = Codec::H264;
    Accel accel = Accel::Software;

    friend bool operator==(const EncoderInfo&, const EncoderInfo&) = default;
};

[[nodiscard]] Result<void> check_video_size(std::uint32_t width, std::uint32_t height);
[[nodiscard]] Result<void> check_picture(std::span<const std::uint8_t> rgba, std::uint32_t width, std::uint32_t height);

// One coded picture: an Annex B access unit.
struct AccessUnit {
    Bytes data;
    bool keyframe = false;

    friend bool operator==(const AccessUnit&, const AccessUnit&) = default;
};

// The encoder port: RGBA in, one access unit per picture out, no B-frames.
class VideoEncoder {
  public:
    virtual ~VideoEncoder() = default;
    [[nodiscard]] virtual Result<AccessUnit> encode(std::span<const std::uint8_t> rgba, bool keyframe) = 0;
};

// Codes pictures into frames, skipping unchanged ones.
class LiveEncoder {
  public:
    LiveEncoder(std::pair<std::uint32_t, std::uint32_t> size, Codec codec, std::unique_ptr<VideoEncoder> encoder);

    [[nodiscard]] std::pair<std::uint32_t, std::uint32_t> size() const noexcept { return size_; }
    void request_keyframe() noexcept { keyframe_pending_ = true; }
    // `std::nullopt` when nothing changed. The caller numbers the frames.
    [[nodiscard]] Result<std::optional<Frame>> encode(std::span<const std::uint8_t> rgba, std::uint64_t input_seq,
                                                      std::uint64_t captured_us);

  private:
    std::pair<std::uint32_t, std::uint32_t> size_;
    Codec codec_;
    std::unique_ptr<VideoEncoder> encoder_;
    std::vector<std::uint8_t> last_;
    bool keyframe_pending_ = true;
};

// `error` says why the encoder can't code.
struct ProbeResult {
    EncoderInfo info;
    std::optional<std::string> error;

    [[nodiscard]] bool works() const noexcept { return !error.has_value(); }
    friend bool operator==(const ProbeResult&, const ProbeResult&) = default;
};

struct FfmpegConfig;

// Every encoder this machine could use; software always comes last.
[[nodiscard]] std::vector<ProbeResult> probe(const FfmpegConfig* ffmpeg);

// Start codes of an Annex B buffer: where each code starts and where its NAL unit starts.
[[nodiscard]] std::vector<std::pair<std::size_t, std::size_t>> start_codes(std::span<const std::uint8_t> stream);
// Types of the non-empty NAL units of an access unit.
[[nodiscard]] std::vector<std::uint8_t> nal_types(Codec codec, std::span<const std::uint8_t> access_unit);
[[nodiscard]] bool is_keyframe(Codec codec, std::span<const std::uint8_t> access_unit);
[[nodiscard]] bool has_picture(Codec codec, std::span<const std::uint8_t> access_unit);
[[nodiscard]] bool has_parameter_sets(Codec codec, std::span<const std::uint8_t> access_unit);

// The software encoder's name stays `rusty_h264`, as Rust peers report it.
[[nodiscard]] EncoderInfo software_info();

// In-process Constrained Baseline H.264 (openh264) for when no hardware encoder works.
class SoftwareH264 final : public VideoEncoder {
  public:
    [[nodiscard]] static Result<std::unique_ptr<SoftwareH264>> make(std::uint32_t width, std::uint32_t height);
    ~SoftwareH264() override;
    SoftwareH264(const SoftwareH264&) = delete;
    SoftwareH264& operator=(const SoftwareH264&) = delete;

    [[nodiscard]] Result<AccessUnit> encode(std::span<const std::uint8_t> rgba, bool keyframe) override;

    struct Impl;

  private:
    explicit SoftwareH264(std::unique_ptr<Impl> impl);
    std::unique_ptr<Impl> impl_;
};

// Tightly packed RGBA to I420 (BT.601, limited range); even sizes only.
void rgba_to_i420(std::span<const std::uint8_t> rgba, std::size_t width, std::size_t height, std::span<std::uint8_t> y,
                  std::span<std::uint8_t> u, std::span<std::uint8_t> v);

} // namespace loa::surfaces::video
