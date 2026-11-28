#include "loa/ui/surface/decode.hpp"

#include <algorithm>
#include <limits>

namespace loa::ui::surface {

std::optional<Framebuffer> Framebuffer::make(std::uint32_t width, std::uint32_t height) {
    const std::size_t max = std::numeric_limits<std::size_t>::max();
    const std::size_t w = width;
    const std::size_t h = height;
    if (h != 0 && w > max / h) return std::nullopt;
    if (w * h > max / 4) return std::nullopt;
    return Framebuffer(width, height, w * h * 4);
}

Chain::Chain(const Decoders& decoders) {
    if (decoders.extra && *decoders.extra) decoders_.push_back((*decoders.extra)());
    if (decoders.still) decoders_.push_back(decoders.still());
    std::erase(decoders_, nullptr);
}

std::vector<Encoding> Chain::accepted() const {
    std::vector<Encoding> out;
    for (const auto& d : decoders_) {
        auto a = d->accepts();
        out.insert(out.end(), a.begin(), a.end());
    }
    return out;
}

std::vector<Encoding> Chain::still() const {
    if (decoders_.empty()) return {};
    auto a = decoders_.back()->accepts();
    return {a.begin(), a.end()};
}

FrameDecoder* Chain::find(Encoding encoding) {
    for (auto& d : decoders_)
        if (std::ranges::find(d->accepts(), encoding) != d->accepts().end()) return d.get();
    return nullptr;
}

void Chain::reset() {
    for (auto& d : decoders_) d->reset();
}

std::expected<bool, DecodeError> apply(FrameDecoder& decoder, const Frame& frame, Framebuffer& fb) {
    return decoder.apply(frame, fb.pixels_mut(), fb.width(), fb.height());
}

bool is_video(Encoding encoding) { return encoding == Encoding::H264 || encoding == Encoding::Hevc; }

std::string_view encoding_name(Encoding encoding) {
    switch (encoding) {
    case Encoding::Rgba: return "RGBA";
    case Encoding::Jpeg: return "JPEG";
    case Encoding::Png: return "PNG";
    case Encoding::H264: return "H.264";
    case Encoding::Hevc: return "HEVC";
    }
    return "RGBA";
}

} // namespace loa::ui::surface
