#include "loa/app/adapters/surface.hpp"

#include "loa/surfaces/docs/format.hpp"
#include "loa/surfaces/pixels.hpp"

namespace loa::app::adapters {

namespace port = view::surface::port;
namespace docs = surfaces::docs;
using view::surface::Encoding;

namespace {

docs::DocPalette doc_palette(const port::DocumentPalette& p) {
    return {.page = {p.page},
            .text = {p.text},
            .rule = {p.rule},
            .grid = {p.grid},
            .header = {p.header},
            .header_text = {p.header_text}};
}

// A parsed document and the fonts its pages are drawn with.
class DocsPages final : public port::PagedDocument {
  public:
    DocsPages(std::unique_ptr<docs::Paged> paged, docs::Fonts fonts) : paged_(std::move(paged)), fonts_(std::move(fonts)) {}

    [[nodiscard]] std::size_t page_count() const override { return paged_->page_count(); }

    [[nodiscard]] std::expected<port::PageSize, std::string> page_size(std::size_t index) const override {
        auto size = paged_->page_size(index);
        if (!size) return std::unexpected(size.error().message());
        return port::PageSize{size->width, size->height};
    }

    [[nodiscard]] std::expected<port::RgbaPage, std::string> render(std::size_t index, float zoom,
                                                                    const port::DocumentPalette& palette) const override {
        docs::RenderOpts opts;
        opts.palette = doc_palette(palette);
        auto page = paged_->render(index, zoom, fonts_, opts);
        if (!page) return std::unexpected(page.error().message());
        return port::RgbaPage{page->width, page->height, std::move(page->data)};
    }

  private:
    std::unique_ptr<docs::Paged> paged_;
    docs::Fonts fonts_;
};

} // namespace

std::span<const Encoding> StillDecoder::accepts() const { return surfaces::STILL_ENCODINGS; }

std::expected<bool, port::DecodeError> StillDecoder::apply(const view::surface::Frame& frame, std::span<std::uint8_t> picture,
                                                           std::uint32_t width, std::uint32_t height) {
    if (auto applied = surfaces::apply_still(picture, width, height, frame); !applied)
        return std::unexpected(port::DecodeError{applied.error().message()});
    return true;
}

#ifdef LOA_SURFACES_REMOTE_VIDEO
namespace {
constexpr std::array<Encoding, 1> H264_ENCODINGS = {Encoding::H264};
} // namespace

std::span<const Encoding> H264Decoder::accepts() const { return H264_ENCODINGS; }

std::expected<bool, port::DecodeError> H264Decoder::apply(const view::surface::Frame& frame, std::span<std::uint8_t> picture,
                                                          std::uint32_t, std::uint32_t) {
    auto decoded = decoder_.decode(frame, picture);
    if (!decoded) return std::unexpected(port::DecodeError{"video decoder: " + decoded.error().message()});
    return *decoded;
}

void H264Decoder::reset() { decoder_.reset(); }
#endif

port::Decoders decoders() {
    port::Decoders out;
#ifdef LOA_SURFACES_REMOTE_VIDEO
    out.extra = [] { return std::make_unique<H264Decoder>(); };
#endif
    out.still = [] { return std::make_unique<StillDecoder>(); };
    return out;
}

std::vector<std::string> DocsRenderer::extensions() const {
    std::vector<std::string> out;
    for (const auto& format : docs::formats())
        for (auto ext : format.exts) out.emplace_back(ext);
    return out;
}

std::expected<port::OpenedDocument, std::string> DocsRenderer::open(const view::surface::Document& document) const {
    if (document.mime.starts_with("image/")) {
        auto size = surfaces::image_size(document.data);
        if (!size) return std::unexpected(size.error().message());
        return port::opened_document::Image{size->first, size->second};
    }
    auto format = docs::from_mime(document.mime);
    if (!format) format = docs::sniff(document.data);
    if (!format) return std::unexpected("no viewer for " + document.mime);
    auto paged = docs::open_pages(*format, document.data);
    if (!paged) return std::unexpected(paged.error().message());
    return port::opened_document::Paged{std::make_unique<DocsPages>(std::move(*paged), fonts_)};
}

} // namespace loa::app::adapters
