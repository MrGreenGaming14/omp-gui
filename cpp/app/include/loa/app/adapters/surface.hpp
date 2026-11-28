#pragma once

// The surface renderer's ports over `loa::surfaces` (port of
// `loa-app/src/adapters/surface.rs`): still (and with live video, H.264) frame
// decoding, and documents parsed and rasterized by the docs page renderers.
//
// `H264Decoder` exists when `loa::surfaces` is built with remote video
// (`LOA_SURFACES_REMOTE_VIDEO`, the Rust feature `remote-video`).

#include <memory>
#include <span>
#include <string>
#include <vector>

#include "loa/surfaces/docs/raster.hpp"
#include "loa/view/surface.hpp"

#ifdef LOA_SURFACES_REMOTE_VIDEO
#include "loa/surfaces/video/decode.hpp"
#endif

namespace loa::app::adapters {

// RGBA, PNG and JPEG regions, written onto the retained picture.
class StillDecoder final : public view::surface::port::FrameDecoder {
  public:
    [[nodiscard]] std::span<const view::surface::Encoding> accepts() const override;
    [[nodiscard]] std::expected<bool, view::surface::port::DecodeError> apply(const view::surface::Frame& frame,
                                                                              std::span<std::uint8_t> picture,
                                                                              std::uint32_t width,
                                                                              std::uint32_t height) override;
};

#ifdef LOA_SURFACES_REMOTE_VIDEO
// Decodes H.264 streams (Constrained Baseline, what the live video encoders send).
class H264Decoder final : public view::surface::port::FrameDecoder {
  public:
    [[nodiscard]] std::span<const view::surface::Encoding> accepts() const override;
    [[nodiscard]] std::expected<bool, view::surface::port::DecodeError> apply(const view::surface::Frame& frame,
                                                                              std::span<std::uint8_t> picture,
                                                                              std::uint32_t width,
                                                                              std::uint32_t height) override;
    void reset() override;

  private:
    surfaces::video::VideoDecoder decoder_;
};
#endif

// The decoders of the app's pixel tabs: H.264 first when live video is built in, then stills.
[[nodiscard]] view::surface::port::Decoders decoders();

// Opens documents with the docs page renderers, drawing text in `fonts`.
class DocsRenderer final : public view::surface::port::DocumentRenderer {
  public:
    explicit DocsRenderer(surfaces::docs::Fonts fonts) : fonts_(std::move(fonts)) {}

    [[nodiscard]] std::vector<std::string> extensions() const override;
    [[nodiscard]] std::expected<view::surface::port::OpenedDocument, std::string> open(
        const view::surface::Document& document) const override;

  private:
    surfaces::docs::Fonts fonts_;
};

} // namespace loa::app::adapters
