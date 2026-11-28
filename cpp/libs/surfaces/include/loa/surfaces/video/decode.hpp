#pragma once

// Rebuilding a surface picture from its frames: stills and in-process H.264
// (one picture per access unit; openh264). HEVC is left to viewers that decode
// it. Port of `loa-surfaces/src/proto/video/decode.rs`.

#include <cstdint>
#include <memory>
#include <span>

#include "loa/surfaces/pixels.hpp"
#include "loa/surfaces/video/encode.hpp"

namespace loa::surfaces::video {

// Stateful H.264 decoder: references persist until `reset`.
class VideoDecoder {
  public:
    VideoDecoder();
    ~VideoDecoder();
    VideoDecoder(VideoDecoder&&) noexcept;
    VideoDecoder& operator=(VideoDecoder&&) noexcept;

    [[nodiscard]] bool supports(Encoding encoding) const noexcept { return encoding == Encoding::H264; }
    // Decodes one whole-picture access unit into `out` (tight RGBA of the
    // frame's size). `false` when the unit carried no picture.
    [[nodiscard]] Result<bool> decode(const Frame& frame, std::span<std::uint8_t> out);
    // Forgets reference pictures; the next unit must be a keyframe.
    void reset();

    struct Impl;

  private:
    std::unique_ptr<Impl> impl_;
};

// Applies frames of any encoding to an `RgbaImage`.
class FrameDecoder {
  public:
    // Applies a frame to `canvas`, resizing it to the frame's picture.
    [[nodiscard]] Result<void> apply(const Frame& frame, RgbaImage& canvas);

  private:
    VideoDecoder video_;
};

} // namespace loa::surfaces::video
