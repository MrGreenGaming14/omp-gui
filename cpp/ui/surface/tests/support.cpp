#include "support.hpp"

#include <asio.hpp>

#include "loa/contracts/surface_channel.hpp"

namespace support {

namespace cs = loa::contracts::surface;

cs::Result<std::shared_ptr<cs::FrameReceiver>> FakeSurface::frames() {
    if (info_.mode != vs::SurfaceMode::Pixel) return std::unexpected(cs::SurfaceError::mode(info_.uri, info_.mode));
    if (!sender) return std::shared_ptr<cs::FrameReceiver>{};
    return sender->subscribe();
}

loa::contracts::Awaitable<cs::Result<void>> FakeSurface::request_keyframe() {
    ++keyframes_;
    co_return cs::Result<void>{};
}

loa::contracts::Awaitable<cs::Result<void>> FakeSurface::input(vs::InputEvent event) {
    std::scoped_lock g(lock_);
    inputs_.push_back(std::move(event));
    co_return cs::Result<void>{};
}

loa::contracts::Awaitable<cs::Result<vs::Document>> FakeSurface::document() {
    if (!document_) co_return std::unexpected(cs::SurfaceError::mode(info_.uri, info_.mode));
    co_return *document_;
}

loa::contracts::Awaitable<cs::Result<cs::Bytes>> FakeSurface::screenshot() { co_return cs::Bytes{}; }

std::vector<std::uint8_t> solid(std::uint32_t width, std::uint32_t height, std::array<std::uint8_t, 4> rgba) {
    std::vector<std::uint8_t> out;
    out.reserve(static_cast<std::size_t>(width) * height * 4);
    for (std::size_t i = 0; i < static_cast<std::size_t>(width) * height; ++i) out.insert(out.end(), rgba.begin(), rgba.end());
    return out;
}

std::span<const vs::Encoding> FakeStill::accepts() const {
    static constexpr std::array ENCODINGS{vs::Encoding::Rgba, vs::Encoding::Png, vs::Encoding::Jpeg};
    return ENCODINGS;
}

std::expected<bool, vs::port::DecodeError> FakeStill::apply(const vs::Frame& frame, std::span<std::uint8_t> picture,
                                                            std::uint32_t width, std::uint32_t height) {
    const auto& r = frame.region;
    if (r.x + r.width > width || r.y + r.height > height || frame.data.size() != static_cast<std::size_t>(r.width) * r.height * 4)
        return std::unexpected(vs::port::DecodeError{"region doesn't fit"});
    const std::size_t row = static_cast<std::size_t>(r.width) * 4;
    for (std::uint32_t y = 0; y < r.height; ++y) {
        const std::size_t at = ((static_cast<std::size_t>(r.y) + y) * width + r.x) * 4;
        std::copy_n(frame.data.begin() + static_cast<std::ptrdiff_t>(y * row), row, picture.begin() + static_cast<std::ptrdiff_t>(at));
    }
    return true;
}

namespace {

class OnePage final : public vs::port::PagedDocument {
  public:
    [[nodiscard]] std::size_t page_count() const override { return 1; }
    [[nodiscard]] std::expected<vs::port::PageSize, std::string> page_size(std::size_t) const override {
        return vs::port::PageSize{300, 200};
    }
    [[nodiscard]] std::expected<vs::port::RgbaPage, std::string> render(std::size_t, float zoom,
                                                                        const vs::port::DocumentPalette& palette) const override {
        const auto width = static_cast<std::uint32_t>(300.0F * zoom);
        const auto height = static_cast<std::uint32_t>(200.0F * zoom);
        return vs::port::RgbaPage{width, height, solid(width, height, palette.page)};
    }
};

} // namespace

std::vector<std::string> FakeDocuments::extensions() const { return {"pdf", "xlsx", "docx", "pptx"}; }

std::expected<vs::port::OpenedDocument, std::string> FakeDocuments::open(const vs::Document& document) const {
    if (document.mime.starts_with("image/")) return vs::port::OpenedDocument{vs::port::opened_document::Image{30, 20}};
    if (document.mime == PAGED_MIME)
        return vs::port::OpenedDocument{vs::port::opened_document::Paged{std::make_unique<OnePage>()}};
    return std::unexpected("no viewer for " + document.mime);
}

loa::ui::surface::Options options() {
    return loa::ui::surface::Options::make(vs::port::Decoders{std::nullopt, [] { return std::make_unique<FakeStill>(); }},
                                           std::make_shared<FakeDocuments>());
}

vs::SurfaceUri uri(std::string_view server, std::string_view name) { return *vs::SurfaceUri::make(server, name); }

} // namespace support
