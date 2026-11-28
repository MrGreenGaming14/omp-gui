// Port of `loa-surfaces/src/proto/video/encode.rs`; `rusty_h264` maps to openh264.

#include "loa/surfaces/video/encode.hpp"

#include <wels/codec_api.h>

#include <algorithm>
#include <array>
#include <cstring>
#include <format>

#include "loa/surfaces/video/ffmpeg.hpp"

namespace loa::surfaces::video {

ProtoError video_error(std::string message) { return ProtoError::video(std::move(message)); }

Encoding encoding(Codec codec) noexcept { return codec == Codec::H264 ? Encoding::H264 : Encoding::Hevc; }

namespace {

constexpr std::array<std::uint8_t, 2> H264_PARAMETER_SETS{7, 8};
constexpr std::array<std::uint8_t, 3> HEVC_PARAMETER_SETS{32, 33, 34};
const Nal H264_NAL{"h264", 0, 0x1f, 9, 5, 5, 1, 5, H264_PARAMETER_SETS};
const Nal HEVC_NAL{"hevc", 1, 0x3f, 35, 16, 23, 0, 31, HEVC_PARAMETER_SETS};

} // namespace

const Nal& nal(Codec codec) noexcept { return codec == Codec::H264 ? H264_NAL : HEVC_NAL; }

Result<void> check_video_size(std::uint32_t width, std::uint32_t height) {
    if (width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 || width > 8192 || height > 8192)
        return std::unexpected(video_error(std::format("{}x{}: 4:2:0 video needs an even, nonzero size up to 8192", width, height)));
    return {};
}

Result<void> check_picture(std::span<const std::uint8_t> rgba, std::uint32_t width, std::uint32_t height) {
    if (rgba.size() != static_cast<std::size_t>(width) * height * 4)
        return std::unexpected(video_error(std::format("{} bytes for a {}x{} picture", rgba.size(), width, height)));
    return {};
}

LiveEncoder::LiveEncoder(std::pair<std::uint32_t, std::uint32_t> size, Codec codec, std::unique_ptr<VideoEncoder> encoder)
    : size_(size), codec_(codec), encoder_(std::move(encoder)) {}

Result<std::optional<Frame>> LiveEncoder::encode(std::span<const std::uint8_t> rgba, std::uint64_t input_seq,
                                                 std::uint64_t captured_us) {
    const auto [width, height] = size_;
    if (auto ok = check_picture(rgba, width, height); !ok) return std::unexpected(ok.error());
    const bool keyframe = std::exchange(keyframe_pending_, false);
    if (!keyframe && std::ranges::equal(last_, rgba)) return std::optional<Frame>{};
    last_.assign(rgba.begin(), rgba.end());
    auto unit = encoder_->encode(rgba, keyframe);
    if (!unit) return std::unexpected(unit.error());
    return std::optional<Frame>(Frame{.seq = 0,
                                      .captured_us = captured_us,
                                      .input_seq = input_seq,
                                      .width = width,
                                      .height = height,
                                      .region = {0, 0, width, height},
                                      .encoding = encoding(codec_),
                                      .keyframe = unit->keyframe,
                                      .data = std::move(unit->data)});
}

std::vector<ProbeResult> probe(const FfmpegConfig* ffmpeg) {
    std::vector<ProbeResult> found = ffmpeg ? video::probe(*ffmpeg) : std::vector<ProbeResult>{};
    found.push_back({software_info(), std::nullopt});
    return found;
}

std::vector<std::pair<std::size_t, std::size_t>> start_codes(std::span<const std::uint8_t> stream) {
    std::vector<std::pair<std::size_t, std::size_t>> codes;
    for (std::size_t at = 0; at + 3 <= stream.size(); ++at) {
        if (stream[at] != 0 || stream[at + 1] != 0 || stream[at + 2] != 1) continue;
        codes.emplace_back(at > 0 && stream[at - 1] == 0 ? at - 1 : at, at + 3);
        at += 2;
    }
    return codes;
}

std::vector<std::uint8_t> nal_types(Codec codec, std::span<const std::uint8_t> access_unit) {
    const auto codes = start_codes(access_unit);
    std::vector<std::uint8_t> types;
    for (std::size_t i = 0; i < codes.size(); ++i) {
        const std::size_t begin = codes[i].second;
        const std::size_t end = i + 1 < codes.size() ? codes[i + 1].first : access_unit.size();
        if (begin >= end) continue;
        const auto unit = access_unit.subspan(begin, end - begin);
        if (std::ranges::any_of(unit, [](std::uint8_t b) { return b != 0; })) types.push_back(nal(codec).type_of(unit[0]));
    }
    return types;
}

bool is_keyframe(Codec codec, std::span<const std::uint8_t> access_unit) {
    const auto& n = nal(codec);
    return std::ranges::any_of(nal_types(codec, access_unit), [&](std::uint8_t t) { return t >= n.keyframe_first && t <= n.keyframe_last; });
}

bool has_picture(Codec codec, std::span<const std::uint8_t> access_unit) {
    const auto& n = nal(codec);
    return std::ranges::any_of(nal_types(codec, access_unit), [&](std::uint8_t t) { return t >= n.picture_first && t <= n.picture_last; });
}

bool has_parameter_sets(Codec codec, std::span<const std::uint8_t> access_unit) {
    const auto types = nal_types(codec, access_unit);
    return std::ranges::all_of(nal(codec).parameter_sets, [&](std::uint8_t t) { return std::ranges::contains(types, t); });
}

EncoderInfo software_info() { return {.name = "rusty_h264", .codec = Codec::H264, .accel = Accel::Software}; }

void rgba_to_i420(std::span<const std::uint8_t> rgba, std::size_t width, std::size_t height, std::span<std::uint8_t> y,
                  std::span<std::uint8_t> u, std::span<std::uint8_t> v) {
    const std::size_t pixels = std::min(rgba.size() / 4, y.size());
    for (std::size_t i = 0; i < pixels; ++i) {
        const int r = rgba[i * 4], g = rgba[i * 4 + 1], b = rgba[i * 4 + 2];
        y[i] = static_cast<std::uint8_t>(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16);
    }
    const std::size_t cw = width / 2;
    for (std::size_t crow = 0; crow < height / 2; ++crow) {
        for (std::size_t cx = 0; cx < cw; ++cx) {
            std::array<int, 3> sum{};
            for (std::size_t row : {2 * crow, 2 * crow + 1}) {
                const std::size_t at = (row * width + 2 * cx) * 4;
                for (std::size_t px = 0; px < 2; ++px)
                    for (std::size_t c = 0; c < 3; ++c) sum[c] += rgba[at + px * 4 + c];
            }
            const int r = (sum[0] + 2) >> 2, g = (sum[1] + 2) >> 2, b = (sum[2] + 2) >> 2;
            u[crow * cw + cx] = static_cast<std::uint8_t>(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128);
            v[crow * cw + cx] = static_cast<std::uint8_t>(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128);
        }
    }
}

struct SoftwareH264::Impl {
    ISVCEncoder* encoder = nullptr;
    std::size_t width = 0;
    std::size_t height = 0;
    std::array<std::vector<std::uint8_t>, 3> planes;

    ~Impl() {
        if (encoder) {
            encoder->Uninitialize();
            WelsDestroySVCEncoder(encoder);
        }
    }
};

SoftwareH264::SoftwareH264(std::unique_ptr<Impl> impl) : impl_(std::move(impl)) {}
SoftwareH264::~SoftwareH264() = default;

Result<std::unique_ptr<SoftwareH264>> SoftwareH264::make(std::uint32_t width, std::uint32_t height) {
    if (auto ok = check_video_size(width, height); !ok) return std::unexpected(ok.error());
    auto impl = std::make_unique<Impl>();
    impl->width = width;
    impl->height = height;
    if (WelsCreateSVCEncoder(&impl->encoder) != 0 || !impl->encoder) {
        impl->encoder = nullptr;
        return std::unexpected(video_error("openh264: creating the encoder failed"));
    }
    // Low latency: CAVLC, no B-frames, no lookahead, one reference.
    SEncParamExt config;
    int quiet = WELS_LOG_QUIET;
    impl->encoder->SetOption(ENCODER_OPTION_TRACE_LEVEL, &quiet);
    impl->encoder->GetDefaultParams(&config);
    config.iUsageType = CAMERA_VIDEO_REAL_TIME;
    config.iPicWidth = static_cast<int>(width);
    config.iPicHeight = static_cast<int>(height);
    config.iRCMode = RC_OFF_MODE; // constant QP (Rust `bitrate = 0`)
    config.iTargetBitrate = 1 << 30;
    config.iMaxBitrate = UNSPECIFIED_BIT_RATE;
    config.fMaxFrameRate = static_cast<float>(FPS);
    config.iTemporalLayerNum = 1;
    config.iSpatialLayerNum = 1;
    config.uiIntraPeriod = KEYFRAME_INTERVAL;
    config.iNumRefFrame = 1;
    config.eSpsPpsIdStrategy = CONSTANT_ID;
    config.bPrefixNalAddingCtrl = false;
    config.iEntropyCodingModeFlag = 0;
    config.bEnableFrameSkip = false;
    config.iMinQp = QP;
    config.iMaxQp = QP;
    config.iMultipleThreadIdc = 1;
    config.bEnableSceneChangeDetect = false;
    config.bEnableBackgroundDetection = false;
    config.bEnableAdaptiveQuant = false;
    config.bEnableDenoise = false;
    config.bEnableLongTermReference = false;
    // Smallest level whose frame size limit (in macroblocks) fits.
    const std::size_t mbs = ((width + 15) / 16) * ((height + 15) / 16);
    const ELevelIdc level = mbs <= 1620 ? LEVEL_3_0 : mbs <= 3600 ? LEVEL_3_1 : mbs <= 5120 ? LEVEL_3_2
                            : mbs <= 8192 ? LEVEL_4_1 : mbs <= 22080 ? LEVEL_5_1 : LEVEL_5_2;
    auto& layer = config.sSpatialLayers[0];
    layer.iVideoWidth = static_cast<int>(width);
    layer.iVideoHeight = static_cast<int>(height);
    layer.fFrameRate = static_cast<float>(FPS);
    layer.iSpatialBitrate = config.iTargetBitrate;
    layer.iMaxSpatialBitrate = UNSPECIFIED_BIT_RATE;
    layer.uiProfileIdc = PRO_BASELINE;
    layer.uiLevelIdc = level;
    layer.iDLayerQp = QP;
    layer.sSliceArgument.uiSliceMode = SM_SINGLE_SLICE;
    if (const int rc = impl->encoder->InitializeExt(&config); rc != cmResultSuccess)
        return std::unexpected(video_error(std::format("openh264: initializing the encoder failed ({})", rc)));
    int format = videoFormatI420;
    impl->encoder->SetOption(ENCODER_OPTION_DATAFORMAT, &format);
    const std::size_t luma = impl->width * impl->height;
    impl->planes = {std::vector<std::uint8_t>(luma), std::vector<std::uint8_t>(luma / 4), std::vector<std::uint8_t>(luma / 4)};
    return std::unique_ptr<SoftwareH264>(new SoftwareH264(std::move(impl)));
}

Result<AccessUnit> SoftwareH264::encode(std::span<const std::uint8_t> rgba, bool keyframe) {
    auto& s = *impl_;
    const auto w = s.width, h = s.height;
    if (auto ok = check_picture(rgba, static_cast<std::uint32_t>(w), static_cast<std::uint32_t>(h)); !ok)
        return std::unexpected(ok.error());
    auto& [y, u, v] = s.planes;
    rgba_to_i420(rgba, w, h, y, u, v);
    SSourcePicture picture{};
    picture.iColorFormat = videoFormatI420;
    picture.iPicWidth = static_cast<int>(w);
    picture.iPicHeight = static_cast<int>(h);
    picture.iStride[0] = static_cast<int>(w);
    picture.iStride[1] = picture.iStride[2] = static_cast<int>(w / 2);
    picture.pData[0] = y.data();
    picture.pData[1] = u.data();
    picture.pData[2] = v.data();
    if (keyframe) s.encoder->ForceIntraFrame(true);
    SFrameBSInfo info{};
    if (const int rc = s.encoder->EncodeFrame(&picture, &info); rc != cmResultSuccess)
        return std::unexpected(video_error(std::format("openh264: encoding failed ({})", rc)));
    Bytes data;
    for (int l = 0; l < info.iLayerNum; ++l) {
        const auto& layer = info.sLayerInfo[l];
        std::size_t len = 0;
        for (int n = 0; n < layer.iNalCount; ++n) len += static_cast<std::size_t>(layer.pNalLengthInByte[n]);
        data.insert(data.end(), layer.pBsBuf, layer.pBsBuf + len);
    }
    const bool key = is_keyframe(Codec::H264, data);
    return AccessUnit{.data = std::move(data), .keyframe = key};
}

} // namespace loa::surfaces::video
