#pragma once

// Images: PNG, JPEG, GIF (first frame), WebP and BMP, one page at their pixel
// size. Port of `loa-surfaces/src/docs/images.rs`.

#include <cstdint>
#include <memory>
#include <span>

#include "loa/surfaces/docs/format.hpp"

namespace loa::surfaces::docs::images {

// Decodes `bytes` as `format`. An animated GIF yields its first frame.
[[nodiscard]] Result<std::unique_ptr<Paged>> open(Format format, std::span<const std::uint8_t> bytes);

// Resizes with the `image` crate's `FilterType::Triangle`.
[[nodiscard]] Rgba resize_triangle(const Rgba& image, std::uint32_t width, std::uint32_t height);

} // namespace loa::surfaces::docs::images
