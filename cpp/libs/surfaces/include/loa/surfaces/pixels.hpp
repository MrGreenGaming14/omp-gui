#pragma once

// RGBA pictures and the still codecs every surface and viewer shares. Port of
// `loa-surfaces/src/pixels.rs`.
//
// Pixels are 8 bit RGBA, straight alpha, row major, no padding: the layout of
// `Encoding::Rgba`.

#include <array>
#include <cstddef>
#include <cstdint>
#include <expected>
#include <optional>
#include <span>
#include <string>
#include <utility>
#include <vector>

#include "loa/contracts/surface.hpp"

namespace loa::surfaces {

using loa::contracts::surface::Bytes;
using loa::contracts::surface::Encoding;
using loa::contracts::surface::Frame;
using loa::contracts::surface::Rect;

using Pixel = std::array<std::uint8_t, 4>;

class PixelError {
  public:
    enum class Kind : std::uint8_t { OutOfBounds, Length, Target, Size, NotStill, Codec };

    [[nodiscard]] static PixelError out_of_bounds(Rect region, std::uint32_t width, std::uint32_t height);
    [[nodiscard]] static PixelError length(std::size_t expected, std::size_t actual);
    [[nodiscard]] static PixelError target(std::uint32_t width, std::uint32_t height, std::size_t expected, std::size_t actual);
    [[nodiscard]] static PixelError size(std::uint32_t got_width, std::uint32_t got_height, std::uint32_t width, std::uint32_t height);
    [[nodiscard]] static PixelError not_still(Encoding encoding);
    [[nodiscard]] static PixelError codec(std::string format, std::string message);

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // The Rust `Display` text.
    [[nodiscard]] std::string message() const;

    friend bool operator==(const PixelError&, const PixelError&) = default;

  private:
    PixelError() = default;
    Kind kind_ = Kind::Codec;
    Rect region_{};
    std::uint64_t a_ = 0, b_ = 0, c_ = 0, d_ = 0; // widths, heights, byte counts per kind
    Encoding encoding_ = Encoding::Rgba;
    std::string format_;
    std::string text_;
};

template <typename T>
using PixelResult = std::expected<T, PixelError>;

// Bytes of a `width` by `height` RGBA picture, `std::nullopt` on overflow.
[[nodiscard]] std::optional<std::size_t> rgba_len(std::uint32_t width, std::uint32_t height);
// Whether `region` lies inside a `width` by `height` picture.
[[nodiscard]] bool region_fits(Rect region, std::uint32_t width, std::uint32_t height);
// Copies the RGBA rows of `src` into `region` of `dst`, a `width` by `height`
// picture. Nothing is written when the sizes don't match.
PixelResult<void> blit(std::span<std::uint8_t> dst, std::uint32_t width, std::uint32_t height,
                       std::span<const std::uint8_t> src, Rect region);
// The first and last row that differ between two pictures of `width`, or
// `std::nullopt` when they are equal.
[[nodiscard]] std::optional<std::pair<std::uint32_t, std::uint32_t>>
changed_rows(std::span<const std::uint8_t> previous, std::span<const std::uint8_t> current, std::uint32_t width);

// An owned RGBA picture.
struct RgbaImage {
    std::uint32_t width = 0;
    std::uint32_t height = 0;
    std::vector<std::uint8_t> data;

    // A picture of one colour.
    [[nodiscard]] static RgbaImage filled(std::uint32_t width, std::uint32_t height, Pixel pixel);
    // Transparent black.
    [[nodiscard]] static RgbaImage make(std::uint32_t width, std::uint32_t height);

    [[nodiscard]] Rect full() const { return {0, 0, width, height}; }
    // The pixel at `(x, y)`.
    [[nodiscard]] std::optional<Pixel> pixel(std::uint32_t x, std::uint32_t y) const;
    // Writes `src`, RGBA rows covering `region`.
    PixelResult<void> blit(Rect region, std::span<const std::uint8_t> src);
    // The pixels of `region`, which lies inside the picture.
    [[nodiscard]] Bytes crop(Rect region) const;
    // Paints `region`, clipped to the picture, with `pixel`.
    void fill(Rect region, Pixel pixel);
    // Copies the `region` sized area at `(src_x, src_y)` to `region`. Both lie
    // inside the picture and may overlap.
    void copy_rect(std::uint32_t src_x, std::uint32_t src_y, Rect region);
    [[nodiscard]] PixelResult<std::vector<std::uint8_t>> to_png() const;

    friend bool operator==(const RgbaImage&, const RgbaImage&) = default;
};

// Encodes RGBA pixels as PNG.
[[nodiscard]] PixelResult<std::vector<std::uint8_t>> encode_png(std::uint32_t width, std::uint32_t height,
                                                                 std::span<const std::uint8_t> rgba);
[[nodiscard]] PixelResult<RgbaImage> decode_png(std::span<const std::uint8_t> data);
// Decodes a baseline or progressive JPEG, including Chrome's screencast
// frames with one scan per component.
[[nodiscard]] PixelResult<RgbaImage> decode_jpeg(std::span<const std::uint8_t> data);

// Encodings `apply_still` decodes.
inline constexpr std::array<Encoding, 3> STILL_ENCODINGS = {Encoding::Rgba, Encoding::Png, Encoding::Jpeg};

// Applies an RGBA, PNG or JPEG frame at its region onto `dst`, a `width` by
// `height` picture. `dst` is left unchanged on error.
PixelResult<void> apply_still(std::span<std::uint8_t> dst, std::uint32_t width, std::uint32_t height, const Frame& frame);

// Width and height from a PNG header.
[[nodiscard]] std::optional<std::pair<std::uint32_t, std::uint32_t>> png_size(std::span<const std::uint8_t> png);
// Width and height of any image format the build decodes (PNG, JPEG, GIF,
// WebP, BMP), from its header.
[[nodiscard]] PixelResult<std::pair<std::uint32_t, std::uint32_t>> image_size(std::span<const std::uint8_t> data);

} // namespace loa::surfaces
