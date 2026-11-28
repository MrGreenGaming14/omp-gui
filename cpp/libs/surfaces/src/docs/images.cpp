#include "loa/surfaces/docs/images.hpp"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <format>

#include <gif_lib.h>
#include <webp/decode.h>

// The one TU with the stb_image implementation, BMP only and private to this file.
#define STB_IMAGE_IMPLEMENTATION
#define STB_IMAGE_STATIC
#define STBI_ONLY_BMP
#define STBI_NO_STDIO
#include <stb/stb_image.h>

namespace loa::surfaces::docs::images {

namespace {

// A decoded image.
class ImageDoc final : public Paged {
  public:
    explicit ImageDoc(Rgba pixels) : pixels_(std::move(pixels)) {}

    [[nodiscard]] std::size_t page_count() const override { return 1; }

    [[nodiscard]] Result<PageSize> page_size(std::size_t index) const override {
        if (index != 0) return std::unexpected(DocsError::page_range(index, 1));
        return PageSize{static_cast<float>(pixels_.width), static_cast<float>(pixels_.height)};
    }

    [[nodiscard]] Result<Rgba> render(std::size_t index, float scale, const Fonts&, const RenderOpts&) const override {
        auto size = page_size(index);
        if (!size) return std::unexpected(std::move(size.error()));
        const auto [width, height] = size->pixels(scale);
        if (width == pixels_.width && height == pixels_.height) return pixels_;
        return resize_triangle(pixels_, width, height);
    }

  private:
    Rgba pixels_;
};

struct GifReader {
    std::span<const std::uint8_t> bytes;
    std::size_t at = 0;
};

int gif_read(GifFileType* gif, GifByteType* out, int len) {
    auto* reader = static_cast<GifReader*>(gif->UserData);
    const std::size_t n = std::min<std::size_t>(static_cast<std::size_t>(len), reader->bytes.size() - reader->at);
    std::memcpy(out, reader->bytes.data() + reader->at, n);
    reader->at += n;
    return static_cast<int>(n);
}

// The first frame on a transparent canvas of the logical screen size.
std::expected<Rgba, std::string> decode_gif(std::span<const std::uint8_t> bytes) {
    GifReader reader{bytes};
    int error = 0;
    GifFileType* gif = DGifOpen(&reader, gif_read, &error);
    if (gif == nullptr) return std::unexpected(std::string(GifErrorString(error)));
    struct Close {
        GifFileType* gif;
        ~Close() { DGifCloseFile(gif, nullptr); }
    } close{gif};
    if (DGifSlurp(gif) != GIF_OK) return std::unexpected(std::string(GifErrorString(gif->Error)));
    if (gif->ImageCount < 1) return std::unexpected(std::string("no image"));
    const SavedImage& frame = gif->SavedImages[0];
    const ColorMapObject* map = frame.ImageDesc.ColorMap != nullptr ? frame.ImageDesc.ColorMap : gif->SColorMap;
    if (map == nullptr) return std::unexpected(std::string("no color map"));
    GraphicsControlBlock gcb{};
    gcb.TransparentColor = NO_TRANSPARENT_COLOR;
    DGifSavedExtensionToGCB(gif, 0, &gcb);
    const auto width = static_cast<std::uint32_t>(std::max(gif->SWidth, 0));
    const auto height = static_cast<std::uint32_t>(std::max(gif->SHeight, 0));
    Rgba out = Rgba::make(width, height);
    const GifImageDesc& d = frame.ImageDesc;
    for (int y = 0; y < d.Height; ++y) {
        for (int x = 0; x < d.Width; ++x) {
            const int index = frame.RasterBits[static_cast<std::size_t>(y) * static_cast<std::size_t>(d.Width) + static_cast<std::size_t>(x)];
            const int px = d.Left + x, py = d.Top + y;
            if (px < 0 || py < 0 || px >= gif->SWidth || py >= gif->SHeight) continue;
            if (index == gcb.TransparentColor || index >= map->ColorCount) continue;
            const GifColorType c = map->Colors[index];
            std::uint8_t* p = out.data.data() + (static_cast<std::size_t>(py) * width + static_cast<std::size_t>(px)) * 4;
            p[0] = c.Red, p[1] = c.Green, p[2] = c.Blue, p[3] = 255;
        }
    }
    return out;
}

std::expected<Rgba, std::string> decode_webp(std::span<const std::uint8_t> bytes) {
    int w = 0, h = 0;
    std::uint8_t* data = WebPDecodeRGBA(bytes.data(), bytes.size(), &w, &h);
    if (data == nullptr) return std::unexpected(std::string("invalid WebP data"));
    Rgba out;
    out.width = static_cast<std::uint32_t>(w);
    out.height = static_cast<std::uint32_t>(h);
    out.data.assign(data, data + static_cast<std::size_t>(w) * static_cast<std::size_t>(h) * 4);
    WebPFree(data);
    return out;
}

std::expected<Rgba, std::string> decode_bmp(std::span<const std::uint8_t> bytes) {
    int w = 0, h = 0, channels = 0;
    if (bytes.size() > static_cast<std::size_t>(INT32_MAX)) return std::unexpected(std::string("file too large"));
    stbi_uc* data = stbi_load_from_memory(bytes.data(), static_cast<int>(bytes.size()), &w, &h, &channels, 4);
    if (data == nullptr) return std::unexpected(std::string(stbi_failure_reason()));
    Rgba out;
    out.width = static_cast<std::uint32_t>(w);
    out.height = static_cast<std::uint32_t>(h);
    out.data.assign(data, data + static_cast<std::size_t>(w) * static_cast<std::size_t>(h) * 4);
    stbi_image_free(data);
    return out;
}

// One pass of the `image` crate's separable resampling along an axis, on float RGBA.
std::vector<float> sample(const std::vector<float>& src, std::uint32_t width, std::uint32_t height, std::uint32_t new_len,
                          bool horizontal) {
    const std::uint32_t len = horizontal ? width : height;
    const std::uint32_t other = horizontal ? height : width;
    const float ratio = static_cast<float>(len) / static_cast<float>(new_len);
    const float sratio = ratio < 1.0F ? 1.0F : ratio;
    const float support = 1.0F * sratio; // Triangle support.
    const std::uint32_t out_w = horizontal ? new_len : width;
    std::vector<float> out(static_cast<std::size_t>(out_w) * (horizontal ? height : new_len) * 4);
    std::vector<float> ws;
    for (std::uint32_t o = 0; o < new_len; ++o) {
        const float center = (static_cast<float>(o) + 0.5F) * ratio;
        const auto left = std::clamp<std::int64_t>(static_cast<std::int64_t>(std::floor(center - support)), 0, std::int64_t{len} - 1);
        const auto right = std::clamp<std::int64_t>(static_cast<std::int64_t>(std::ceil(center + support)), left + 1, len);
        const float c = center - 0.5F;
        ws.clear();
        float sum = 0;
        for (auto i = left; i < right; ++i) {
            const float w = std::max(0.0F, 1.0F - std::abs((static_cast<float>(i) - c) / sratio));
            ws.push_back(w);
            sum += w;
        }
        for (auto& w : ws) w /= sum;
        for (std::uint32_t k = 0; k < other; ++k) {
            float acc[4] = {0, 0, 0, 0};
            for (std::size_t i = 0; i < ws.size(); ++i) {
                const auto s = static_cast<std::uint32_t>(left) + static_cast<std::uint32_t>(i);
                const std::size_t at = horizontal ? (static_cast<std::size_t>(k) * width + s) * 4 : (static_cast<std::size_t>(s) * width + k) * 4;
                for (int ch = 0; ch < 4; ++ch) acc[ch] += src[at + ch] * ws[i];
            }
            const std::size_t at = horizontal ? (static_cast<std::size_t>(k) * out_w + o) * 4 : (static_cast<std::size_t>(o) * out_w + k) * 4;
            for (int ch = 0; ch < 4; ++ch) out[at + ch] = acc[ch];
        }
    }
    return out;
}

} // namespace

Rgba resize_triangle(const Rgba& image, std::uint32_t width, std::uint32_t height) {
    Rgba out = Rgba::make(width, height);
    if (image.width == 0 || image.height == 0 || width == 0 || height == 0) return out;
    std::vector<float> src(image.data.begin(), image.data.end());
    // The `image` crate samples vertically, then horizontally.
    auto tall = sample(src, image.width, image.height, height, false);
    auto wide = sample(tall, image.width, height, width, true);
    for (std::size_t i = 0; i < out.data.size(); ++i) out.data[i] = static_cast<std::uint8_t>(std::round(std::clamp(wide[i], 0.0F, 255.0F)));
    return out;
}

Result<std::unique_ptr<Paged>> open(Format format, std::span<const std::uint8_t> bytes) {
    const DocFormat& s = spec(format);
    if (s.sniff.kind != Sniff::Kind::Image)
        return std::unexpected(DocsError::unsupported(std::format("{} is not an image format", to_string(format))));
    std::expected<Rgba, std::string> image;
    switch (s.sniff.image) {
        case ImageFormat::Png: {
            auto png = decode_png(bytes);
            image = png ? std::expected<Rgba, std::string>(std::move(*png)) : std::unexpected(png.error().message());
            break;
        }
        case ImageFormat::Jpeg: {
            auto jpeg = decode_jpeg(bytes);
            image = jpeg ? std::expected<Rgba, std::string>(std::move(*jpeg)) : std::unexpected(jpeg.error().message());
            break;
        }
        case ImageFormat::Gif: image = decode_gif(bytes); break;
        case ImageFormat::WebP: image = decode_webp(bytes); break;
        case ImageFormat::Bmp: image = decode_bmp(bytes); break;
    }
    if (!image) return std::unexpected(DocsError::invalid(format, std::move(image.error())));
    return std::unique_ptr<Paged>(std::make_unique<ImageDoc>(std::move(*image)));
}

} // namespace loa::surfaces::docs::images
