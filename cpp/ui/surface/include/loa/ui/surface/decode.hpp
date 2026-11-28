#pragma once

// Turning frames into pixels: a chain of the app's `FrameDecoder`s with its still decoder last.
// What the chain accepts is what a tab negotiates with its surface. Port of
// `crates/loa-ui-surface/src/decode.rs`.

#include <expected>
#include <memory>
#include <string_view>
#include <vector>

#include "loa/ui/surface/framebuffer.hpp"
#include "loa/view/surface.hpp"

namespace loa::ui::surface {

using loa::view::surface::Encoding;
using loa::view::surface::Frame;
using FrameDecoder = loa::view::surface::port::FrameDecoder;
using DecodeError = loa::view::surface::port::DecodeError;
using Decoders = loa::view::surface::port::Decoders;

// The decoders of one tab: the extra one first, then the still one.
class Chain {
  public:
    explicit Chain(const Decoders& decoders);

    // What the chain decodes, best first.
    [[nodiscard]] std::vector<Encoding> accepted() const;
    // What the still decoder (last in the chain) decodes: asked for when a stream can't be.
    [[nodiscard]] std::vector<Encoding> still() const;
    // The first decoder accepting `encoding`, or null.
    [[nodiscard]] FrameDecoder* find(Encoding encoding);
    void reset();

  private:
    std::vector<std::unique_ptr<FrameDecoder>> decoders_;
};

// Applies `frame` onto `fb` with `decoder`.
[[nodiscard]] std::expected<bool, DecodeError> apply(FrameDecoder& decoder, const Frame& frame, Framebuffer& fb);

// Whether frames of `encoding` reference earlier ones, so a lost frame needs a keyframe.
[[nodiscard]] bool is_video(Encoding encoding);
// A short name for messages on the tab.
[[nodiscard]] std::string_view encoding_name(Encoding encoding);

} // namespace loa::ui::surface
