#include "loa/surfaces/pixels.hpp"

#include <png.h>

#include <algorithm>
#include <csetjmp>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <format>
#include <limits>

#include <jpeglib.h>

namespace loa::surfaces {

namespace {

std::string_view encoding_name(Encoding encoding) {
    switch (encoding) {
    case Encoding::Rgba: return "Rgba";
    case Encoding::Jpeg: return "Jpeg";
    case Encoding::Png: return "Png";
    case Encoding::H264: return "H264";
    case Encoding::Hevc: return "Hevc";
    }
    return "Unknown";
}

std::string rect_debug(Rect r) {
    return std::format("Rect {{ x: {}, y: {}, width: {}, height: {} }}", r.x, r.y, r.width, r.height);
}

std::uint32_t be32(std::span<const std::uint8_t> d, std::size_t at) {
    return std::uint32_t(d[at]) << 24 | std::uint32_t(d[at + 1]) << 16 | std::uint32_t(d[at + 2]) << 8 | d[at + 3];
}
std::uint32_t be16(std::span<const std::uint8_t> d, std::size_t at) { return std::uint32_t(d[at]) << 8 | d[at + 1]; }
std::uint32_t le16(std::span<const std::uint8_t> d, std::size_t at) { return std::uint32_t(d[at + 1]) << 8 | d[at]; }
std::uint32_t le24(std::span<const std::uint8_t> d, std::size_t at) { return le16(d, at) | std::uint32_t(d[at + 2]) << 16; }
std::uint32_t le32(std::span<const std::uint8_t> d, std::size_t at) { return le16(d, at) | le16(d, at + 2) << 16; }

} // namespace

PixelError PixelError::out_of_bounds(Rect region, std::uint32_t width, std::uint32_t height) {
    PixelError e;
    e.kind_ = Kind::OutOfBounds;
    e.region_ = region;
    e.a_ = width;
    e.b_ = height;
    return e;
}

PixelError PixelError::length(std::size_t expected, std::size_t actual) {
    PixelError e;
    e.kind_ = Kind::Length;
    e.a_ = expected;
    e.b_ = actual;
    return e;
}

PixelError PixelError::target(std::uint32_t width, std::uint32_t height, std::size_t expected, std::size_t actual) {
    PixelError e;
    e.kind_ = Kind::Target;
    e.a_ = width;
    e.b_ = height;
    e.c_ = expected;
    e.d_ = actual;
    return e;
}

PixelError PixelError::size(std::uint32_t got_width, std::uint32_t got_height, std::uint32_t width, std::uint32_t height) {
    PixelError e;
    e.kind_ = Kind::Size;
    e.a_ = got_width;
    e.b_ = got_height;
    e.c_ = width;
    e.d_ = height;
    return e;
}

PixelError PixelError::not_still(Encoding encoding) {
    PixelError e;
    e.kind_ = Kind::NotStill;
    e.encoding_ = encoding;
    return e;
}

PixelError PixelError::codec(std::string format, std::string message) {
    PixelError e;
    e.kind_ = Kind::Codec;
    e.format_ = std::move(format);
    e.text_ = std::move(message);
    return e;
}

std::string PixelError::message() const {
    switch (kind_) {
    case Kind::OutOfBounds: return std::format("region {} is outside the {}x{} picture", rect_debug(region_), a_, b_);
    case Kind::Length: return std::format("region needs {} bytes of RGBA, got {}", a_, b_);
    case Kind::Target: return std::format("a {}x{} picture needs {} bytes, it has {}", a_, b_, c_, d_);
    case Kind::Size: return std::format("decoded picture is {}x{}, its region is {}x{}", a_, b_, c_, d_);
    case Kind::NotStill: return std::format("{} is not a still encoding", encoding_name(encoding_));
    case Kind::Codec: return std::format("{}: {}", format_, text_);
    }
    return text_;
}

std::optional<std::size_t> rgba_len(std::uint32_t width, std::uint32_t height) {
    std::size_t n = 0;
    if (__builtin_mul_overflow(std::size_t(width), std::size_t(height), &n)) return std::nullopt;
    if (__builtin_mul_overflow(n, std::size_t(4), &n)) return std::nullopt;
    return n;
}

bool region_fits(Rect region, std::uint32_t width, std::uint32_t height) {
    return std::uint64_t(region.x) + region.width <= width && std::uint64_t(region.y) + region.height <= height;
}

PixelResult<void> blit(std::span<std::uint8_t> dst, std::uint32_t width, std::uint32_t height,
                       std::span<const std::uint8_t> src, Rect region) {
    const auto expected = rgba_len(width, height).value_or(std::numeric_limits<std::size_t>::max());
    if (dst.size() != expected) return std::unexpected(PixelError::target(width, height, expected, dst.size()));
    if (!region_fits(region, width, height)) return std::unexpected(PixelError::out_of_bounds(region, width, height));
    // The region fits inside a picture whose byte length fits in size_t.
    const std::size_t row = std::size_t(region.width) * 4;
    if (src.size() != row * region.height) return std::unexpected(PixelError::length(row * region.height, src.size()));
    if (row == 0) return {};
    const std::size_t stride = std::size_t(width) * 4;
    for (std::size_t i = 0; i < region.height; ++i) {
        const std::size_t start = (region.y + i) * stride + std::size_t(region.x) * 4;
        std::memcpy(dst.data() + start, src.data() + i * row, row);
    }
    return {};
}

std::optional<std::pair<std::uint32_t, std::uint32_t>>
changed_rows(std::span<const std::uint8_t> previous, std::span<const std::uint8_t> current, std::uint32_t width) {
    const std::size_t row = std::max<std::size_t>(std::size_t(width) * 4, 1);
    const std::size_t rows = current.size() / row;
    const auto changed = [&](std::size_t r) {
        const auto cur = current.subspan(r * row, row);
        if ((r + 1) * row > previous.size()) return true; // Rust: `None != Some(..)`
        return !std::ranges::equal(cur, previous.subspan(r * row, row));
    };
    std::size_t first = 0;
    while (first < rows && !changed(first)) ++first;
    if (first == rows) return std::nullopt;
    std::size_t last = rows - 1;
    while (last > first && !changed(last)) --last;
    return std::pair{std::uint32_t(first), std::uint32_t(last)};
}

RgbaImage RgbaImage::filled(std::uint32_t width, std::uint32_t height, Pixel pixel) {
    RgbaImage image{width, height, {}};
    const std::size_t n = std::size_t(width) * height;
    image.data.resize(n * 4);
    for (std::size_t i = 0; i < n; ++i) std::memcpy(image.data.data() + i * 4, pixel.data(), 4);
    return image;
}

RgbaImage RgbaImage::make(std::uint32_t width, std::uint32_t height) { return filled(width, height, {0, 0, 0, 0}); }

std::optional<Pixel> RgbaImage::pixel(std::uint32_t x, std::uint32_t y) const {
    if (x >= width || y >= height) return std::nullopt;
    const std::size_t i = (std::size_t(y) * width + x) * 4;
    return Pixel{data[i], data[i + 1], data[i + 2], data[i + 3]};
}

PixelResult<void> RgbaImage::blit(Rect region, std::span<const std::uint8_t> src) {
    return surfaces::blit(data, width, height, src, region);
}

Bytes RgbaImage::crop(Rect region) const {
    if (region == full()) return data;
    const std::size_t row = std::size_t(region.width) * 4, stride = std::size_t(width) * 4;
    Bytes out;
    out.reserve(row * region.height);
    for (std::size_t y = region.y; y < std::size_t(region.y) + region.height; ++y) {
        const auto* start = data.data() + y * stride + std::size_t(region.x) * 4;
        out.insert(out.end(), start, start + row);
    }
    return out;
}

void RgbaImage::fill(Rect region, Pixel pixel) {
    const auto sat = [](std::uint32_t a, std::uint32_t b) {
        return std::uint64_t(a) + b > std::numeric_limits<std::uint32_t>::max() ? std::numeric_limits<std::uint32_t>::max() : a + b;
    };
    const std::size_t x1 = std::min(sat(region.x, region.width), width);
    const std::size_t y1 = std::min(sat(region.y, region.height), height);
    for (std::size_t y = region.y; y < y1; ++y) {
        const std::size_t row = y * width;
        for (std::size_t x = region.x; x < x1; ++x) std::memcpy(data.data() + (row + x) * 4, pixel.data(), 4);
    }
}

void RgbaImage::copy_rect(std::uint32_t src_x, std::uint32_t src_y, Rect region) {
    const std::size_t row = std::size_t(region.width) * 4, stride = std::size_t(width) * 4;
    const auto copy = [&](std::size_t i) {
        const std::size_t from = (src_y + i) * stride + std::size_t(src_x) * 4;
        const std::size_t to = (region.y + i) * stride + std::size_t(region.x) * 4;
        std::memmove(data.data() + to, data.data() + from, row);
    };
    if (region.y > src_y) {
        for (std::size_t i = region.height; i-- > 0;) copy(i);
    } else {
        for (std::size_t i = 0; i < region.height; ++i) copy(i);
    }
}

PixelResult<std::vector<std::uint8_t>> RgbaImage::to_png() const { return encode_png(width, height, data); }

PixelResult<std::vector<std::uint8_t>> encode_png(std::uint32_t width, std::uint32_t height,
                                                  std::span<const std::uint8_t> rgba) {
    const auto expected = rgba_len(width, height);
    if (!expected || *expected != rgba.size())
        return std::unexpected(PixelError::codec(
            "PNG", std::format("The provided buffer length {} does not match the {}x{} RGBA picture", rgba.size(), width, height)));
    png_image image{};
    image.version = PNG_IMAGE_VERSION;
    image.width = width;
    image.height = height;
    image.format = PNG_FORMAT_RGBA;
    png_alloc_size_t size = 0;
    if (!png_image_write_to_memory(&image, nullptr, &size, 0, rgba.data(), 0, nullptr)) {
        std::string message = image.message;
        png_image_free(&image);
        return std::unexpected(PixelError::codec("PNG", std::move(message)));
    }
    std::vector<std::uint8_t> out(size);
    if (!png_image_write_to_memory(&image, out.data(), &size, 0, rgba.data(), 0, nullptr)) {
        std::string message = image.message;
        png_image_free(&image);
        return std::unexpected(PixelError::codec("PNG", std::move(message)));
    }
    out.resize(size);
    return out;
}

PixelResult<RgbaImage> decode_png(std::span<const std::uint8_t> data) {
    png_image image{};
    image.version = PNG_IMAGE_VERSION;
    if (!png_image_begin_read_from_memory(&image, data.data(), data.size())) {
        std::string message = image.message;
        png_image_free(&image);
        return std::unexpected(PixelError::codec("PNG", std::move(message)));
    }
    image.format = PNG_FORMAT_RGBA;
    const auto len = rgba_len(image.width, image.height);
    if (!len) {
        png_image_free(&image);
        return std::unexpected(PixelError::codec("PNG", "picture too large"));
    }
    RgbaImage out{image.width, image.height, std::vector<std::uint8_t>(*len)};
    if (!png_image_finish_read(&image, nullptr, out.data.data(), 0, nullptr)) {
        std::string message = image.message;
        png_image_free(&image);
        return std::unexpected(PixelError::codec("PNG", std::move(message)));
    }
    return out;
}

namespace {

struct JpegError {
    jpeg_error_mgr mgr;
    std::jmp_buf jump;
    char message[JMSG_LENGTH_MAX];
};

void jpeg_fail(j_common_ptr info) {
    auto* error = reinterpret_cast<JpegError*>(info->err);
    (*info->err->format_message)(info, error->message);
    std::longjmp(error->jump, 1);
}

void jpeg_quiet(j_common_ptr, int) {}

// Decodes into `out` (sized here) with no C++ objects created between setjmp
// and the last libjpeg call. Returns false with `error->message` set.
bool jpeg_decode_raw(std::span<const std::uint8_t> data, JpegError* error, jpeg_decompress_struct* info,
                     std::vector<std::uint8_t>* out, int* components, J_COLOR_SPACE* space) {
    info->err = jpeg_std_error(&error->mgr);
    error->mgr.error_exit = jpeg_fail;
    error->mgr.emit_message = jpeg_quiet;
    if (setjmp(error->jump)) {
        jpeg_destroy_decompress(info);
        return false;
    }
    jpeg_create_decompress(info);
    jpeg_mem_src(info, data.data(), static_cast<unsigned long>(data.size()));
    jpeg_read_header(info, TRUE);
    switch (info->jpeg_color_space) {
    case JCS_GRAYSCALE: info->out_color_space = JCS_GRAYSCALE; break;
    case JCS_CMYK:
    case JCS_YCCK: info->out_color_space = JCS_CMYK; break;
    default: info->out_color_space = JCS_EXT_RGBA; break;
    }
    jpeg_start_decompress(info);
    *components = info->output_components;
    *space = info->out_color_space;
    const std::size_t stride = std::size_t(info->output_width) * info->output_components;
    out->resize(stride * info->output_height); // may throw bad_alloc; no longjmp in between
    while (info->output_scanline < info->output_height) {
        JSAMPROW row = out->data() + std::size_t(info->output_scanline) * stride;
        jpeg_read_scanlines(info, &row, 1);
    }
    jpeg_finish_decompress(info);
    return true;
}

} // namespace

PixelResult<RgbaImage> decode_jpeg(std::span<const std::uint8_t> data) {
    JpegError error{};
    jpeg_decompress_struct info{};
    std::vector<std::uint8_t> raw;
    int components = 0;
    J_COLOR_SPACE space = JCS_UNKNOWN;
    if (!jpeg_decode_raw(data, &error, &info, &raw, &components, &space))
        return std::unexpected(PixelError::codec("JPEG", error.message));
    const std::uint32_t width = info.output_width, height = info.output_height;
    jpeg_destroy_decompress(&info);
    std::vector<std::uint8_t> rgba;
    if (space == JCS_EXT_RGBA) {
        rgba = std::move(raw);
    } else if (space == JCS_GRAYSCALE) {
        rgba.reserve(raw.size() * 4);
        for (auto g : raw) rgba.insert(rgba.end(), {g, g, g, 255});
    } else { // CMYK as libjpeg stores Adobe's inverted channels.
        rgba.reserve(raw.size());
        for (std::size_t i = 0; i + 4 <= raw.size(); i += 4) {
            const auto channel = [&](std::uint8_t v) { return std::uint8_t((unsigned(v) * raw[i + 3]) / 255); };
            rgba.insert(rgba.end(), {channel(raw[i]), channel(raw[i + 1]), channel(raw[i + 2]), 255});
        }
    }
    (void)components;
    if (rgba_len(width, height) != rgba.size()) return std::unexpected(PixelError::codec("JPEG", "short picture"));
    return RgbaImage{width, height, std::move(rgba)};
}

PixelResult<void> apply_still(std::span<std::uint8_t> dst, std::uint32_t width, std::uint32_t height, const Frame& frame) {
    const Rect region = frame.region;
    PixelResult<RgbaImage> picture;
    switch (frame.encoding) {
    case Encoding::Rgba: return blit(dst, width, height, frame.data, region);
    case Encoding::Png: picture = decode_png(frame.data); break;
    case Encoding::Jpeg: picture = decode_jpeg(frame.data); break;
    default: return std::unexpected(PixelError::not_still(frame.encoding));
    }
    if (!picture) return std::unexpected(std::move(picture.error()));
    if (picture->width != region.width || picture->height != region.height)
        return std::unexpected(PixelError::size(picture->width, picture->height, region.width, region.height));
    return blit(dst, width, height, picture->data, region);
}

std::optional<std::pair<std::uint32_t, std::uint32_t>> png_size(std::span<const std::uint8_t> png) {
    static constexpr std::uint8_t magic[8] = {0x89, 'P', 'N', 'G', '\r', '\n', 0x1a, '\n'};
    if (png.size() < 24 || std::memcmp(png.data(), magic, 8) != 0 || std::memcmp(png.data() + 12, "IHDR", 4) != 0)
        return std::nullopt;
    return std::pair{be32(png, 16), be32(png, 20)};
}

PixelResult<std::pair<std::uint32_t, std::uint32_t>> image_size(std::span<const std::uint8_t> d) {
    const auto error = [](std::string message) { return std::unexpected(PixelError::codec("image", std::move(message))); };
    const auto starts = [&](std::string_view s, std::size_t at = 0) {
        return d.size() >= at + s.size() && std::memcmp(d.data() + at, s.data(), s.size()) == 0;
    };
    const auto truncated = [&] { return error("Format error decoding image: unexpected end of header"); };
    if (starts("\x89PNG\r\n\x1a\n")) {
        if (auto size = png_size(d)) return *size;
        return truncated();
    }
    if (starts("\xff\xd8\xff")) {
        std::size_t at = 2;
        while (at + 4 <= d.size()) {
            if (d[at] != 0xff) return error("Format error decoding Jpeg: invalid marker");
            const std::uint8_t marker = d[at + 1];
            if (marker == 0xff) { ++at; continue; }
            if (marker == 0xd8 || marker == 0x01 || (marker >= 0xd0 && marker <= 0xd7)) { at += 2; continue; }
            const std::uint32_t len = be16(d, at + 2);
            const bool sof = marker >= 0xc0 && marker <= 0xcf && marker != 0xc4 && marker != 0xc8 && marker != 0xcc;
            if (sof) {
                if (at + 9 > d.size()) break;
                return std::pair{be16(d, at + 7), be16(d, at + 5)};
            }
            at += 2 + len;
        }
        return truncated();
    }
    if (starts("GIF87a") || starts("GIF89a")) {
        if (d.size() < 10) return truncated();
        return std::pair{le16(d, 6), le16(d, 8)};
    }
    if (starts("RIFF") && starts("WEBP", 8)) {
        if (starts("VP8 ", 12) && d.size() >= 30) return std::pair{le16(d, 26) & 0x3fff, le16(d, 28) & 0x3fff};
        if (starts("VP8L", 12) && d.size() >= 25) {
            const std::uint32_t bits = le32(d, 21);
            return std::pair{(bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1};
        }
        if (starts("VP8X", 12) && d.size() >= 30) return std::pair{le24(d, 24) + 1, le24(d, 27) + 1};
        return truncated();
    }
    if (starts("BM")) {
        if (d.size() < 26) return truncated();
        if (le32(d, 14) == 12) return std::pair{le16(d, 18), le16(d, 20)};
        const auto h = static_cast<std::int32_t>(le32(d, 22));
        return std::pair{le32(d, 18), static_cast<std::uint32_t>(h < 0 ? -std::int64_t(h) : h)};
    }
    return error("The image format could not be determined");
}

} // namespace loa::surfaces
