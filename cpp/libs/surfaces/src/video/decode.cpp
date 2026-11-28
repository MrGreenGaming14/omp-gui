// Port of `loa-surfaces/src/proto/video/decode.rs`; `rusty_h264` maps to openh264.

#include "loa/surfaces/video/decode.hpp"

#include <wels/codec_api.h>

#include <algorithm>
#include <format>

namespace loa::surfaces::video {

namespace {

std::string_view encoding_name(Encoding encoding) {
    switch (encoding) {
    case Encoding::Rgba: return "Rgba";
    case Encoding::Jpeg: return "Jpeg";
    case Encoding::Png: return "Png";
    case Encoding::H264: return "H264";
    case Encoding::Hevc: return "Hevc";
    }
    return "?";
}

} // namespace

struct VideoDecoder::Impl {
    ISVCDecoder* decoder = nullptr;

    Impl() {
        if (WelsCreateDecoder(&decoder) != 0 || !decoder) {
            decoder = nullptr;
            return;
        }
        int quiet = WELS_LOG_QUIET;
        decoder->SetOption(DECODER_OPTION_TRACE_LEVEL, &quiet);
        SDecodingParam param{};
        param.sVideoProperty.size = sizeof(param.sVideoProperty);
        param.sVideoProperty.eVideoBsType = VIDEO_BITSTREAM_AVC;
        param.eEcActiveIdc = ERROR_CON_DISABLE;
        if (decoder->Initialize(&param) != cmResultSuccess) {
            WelsDestroyDecoder(decoder);
            decoder = nullptr;
        }
    }
    ~Impl() {
        if (decoder) {
            decoder->Uninitialize();
            WelsDestroyDecoder(decoder);
        }
    }
};

VideoDecoder::VideoDecoder() : impl_(std::make_unique<Impl>()) {}
VideoDecoder::~VideoDecoder() = default;
VideoDecoder::VideoDecoder(VideoDecoder&&) noexcept = default;
VideoDecoder& VideoDecoder::operator=(VideoDecoder&&) noexcept = default;

void VideoDecoder::reset() { impl_ = std::make_unique<Impl>(); }

Result<bool> VideoDecoder::decode(const Frame& frame, std::span<std::uint8_t> out) {
    const std::size_t w = frame.width, h = frame.height;
    if (frame.region != Rect{0, 0, frame.width, frame.height} || out.size() != w * h * 4)
        return std::unexpected(video_error("video frames must cover the whole picture"));
    if (frame.encoding != Encoding::H264)
        return std::unexpected(video_error(std::format("no decoder for {} frames", encoding_name(frame.encoding))));
    if (!impl_->decoder) return std::unexpected(video_error("openh264: creating the decoder failed"));
    unsigned char* planes[3] = {nullptr, nullptr, nullptr};
    SBufferInfo info{};
    const auto rc = impl_->decoder->DecodeFrameNoDelay(frame.data.data(), static_cast<int>(frame.data.size()), planes, &info);
    if (rc != dsErrorFree) return std::unexpected(video_error(std::format("openh264: decoding failed ({:#x})", static_cast<int>(rc))));
    if (info.iBufferStatus != 1 || !planes[0]) return false;
    const auto& buffer = info.UsrData.sSystemBuffer;
    const std::size_t pw = static_cast<std::size_t>(buffer.iWidth), ph = static_cast<std::size_t>(buffer.iHeight);
    if (pw != w || ph != h) return std::unexpected(video_error(std::format("picture is {}x{}, the frame {}x{}", pw, ph, w, h)));
    const auto ys = static_cast<std::size_t>(buffer.iStride[0]), cs = static_cast<std::size_t>(buffer.iStride[1]);
    auto clamp = [](int v) { return static_cast<std::uint8_t>(std::clamp(v >> 8, 0, 255)); };
    for (std::size_t row = 0; row < h; ++row) {
        const unsigned char* yrow = planes[0] + row * ys;
        const unsigned char* urow = planes[1] + (row / 2) * cs;
        const unsigned char* vrow = planes[2] + (row / 2) * cs;
        std::uint8_t* dst = out.data() + row * w * 4;
        for (std::size_t x = 0; x < w; ++x) {
            const int c = 298 * (static_cast<int>(yrow[x]) - 16);
            const int d = static_cast<int>(urow[x / 2]) - 128, e = static_cast<int>(vrow[x / 2]) - 128;
            dst[x * 4] = clamp(c + 409 * e + 128);
            dst[x * 4 + 1] = clamp(c - 100 * d - 208 * e + 128);
            dst[x * 4 + 2] = clamp(c + 516 * d + 128);
            dst[x * 4 + 3] = 255;
        }
    }
    return true;
}

Result<void> FrameDecoder::apply(const Frame& frame, RgbaImage& canvas) {
    const bool empty = std::min(frame.region.width, frame.region.height) == 0;
    if (empty || std::max(frame.width, frame.height) > 8192 || !region_fits(frame.region, frame.width, frame.height))
        return std::unexpected(video_error("frame region is outside its picture"));
    if (frame.width != canvas.width || frame.height != canvas.height) canvas = RgbaImage::make(frame.width, frame.height);
    switch (frame.encoding) {
    case Encoding::H264:
    case Encoding::Hevc: {
        auto decoded = video_.decode(frame, canvas.data);
        if (!decoded) return std::unexpected(decoded.error());
        if (!*decoded) return std::unexpected(video_error("access unit contains no picture"));
        return {};
    }
    default: {
        auto applied = apply_still(canvas.data, canvas.width, canvas.height, frame);
        if (!applied) return std::unexpected(video_error(applied.error().message()));
        return {};
    }
    }
}

} // namespace loa::surfaces::video
