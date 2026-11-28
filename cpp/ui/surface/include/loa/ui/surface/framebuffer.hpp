#pragma once

// The retained picture of a pixel surface that region updates are applied to. Port of
// `crates/loa-ui-surface/src/framebuffer.rs`; the view wraps the pixels in a `QImage` without
// copying while it paints.

#include <cstdint>
#include <optional>
#include <span>
#include <vector>

namespace loa::ui::surface {

// A full surface RGBA picture (8 bit, row major, no padding).
class Framebuffer {
  public:
    // A transparent black picture, `nullopt` when the size overflows memory.
    [[nodiscard]] static std::optional<Framebuffer> make(std::uint32_t width, std::uint32_t height);

    [[nodiscard]] std::uint32_t width() const noexcept { return width_; }
    [[nodiscard]] std::uint32_t height() const noexcept { return height_; }
    [[nodiscard]] std::span<const std::uint8_t> pixels() const noexcept { return pixels_; }
    [[nodiscard]] std::span<std::uint8_t> pixels_mut() noexcept { return pixels_; }

  private:
    Framebuffer(std::uint32_t width, std::uint32_t height, std::size_t len) : width_(width), height_(height), pixels_(len) {}
    std::uint32_t width_;
    std::uint32_t height_;
    std::vector<std::uint8_t> pixels_;
};

} // namespace loa::ui::surface
