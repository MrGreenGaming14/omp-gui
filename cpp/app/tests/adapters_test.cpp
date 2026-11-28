// The app's port adapters: still frame decoding and documents over `loa::surfaces`, and the
// node's view of the session library. Port of the tests in `loa-app/src/adapters`.

#include <algorithm>
#include <array>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <tuple>

#include <QBuffer>
#include <QColor>
#include <QImage>

#include <doctest/doctest.h>

#include "loa/app/adapters/peer_sessions.hpp"
#include "loa/app/adapters/surface.hpp"
#include "loa/bus/bus.hpp"
#include "loa/bus/store.hpp"
#include "loa/core/schema.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/session/library.hpp"
#include "loa/surfaces/docs/format.hpp"
#include "loa/surfaces/docs/server.hpp"
#include "loa/surfaces/pixels.hpp"

namespace adapters = loa::app::adapters;
namespace port = loa::view::surface::port;
namespace docs = loa::surfaces::docs;
namespace fs = std::filesystem;
using loa::surfaces::RgbaImage;
using loa::view::surface::Document;
using loa::view::surface::Encoding;
using loa::view::surface::Frame;
using loa::view::surface::Rect;

namespace {

Frame frame(std::uint64_t seq, Rect region, Encoding encoding, std::vector<std::uint8_t> data) {
    const bool keyframe = region == Rect{0, 0, 8, 8};
    return Frame{.seq = seq,
                 .captured_us = 0,
                 .input_seq = 0,
                 .width = 8,
                 .height = 8,
                 .region = region,
                 .encoding = encoding,
                 .keyframe = keyframe,
                 .data = std::move(data)};
}

std::array<std::uint8_t, 4> pixel_at(const std::vector<std::uint8_t>& picture, std::size_t x, std::size_t y) {
    const auto i = (y * 8 + x) * 4;
    return {picture[i], picture[i + 1], picture[i + 2], picture[i + 3]};
}

std::vector<std::uint8_t> png(std::uint32_t width, std::uint32_t height, loa::surfaces::Pixel pixel) {
    auto encoded = RgbaImage::filled(width, height, pixel).to_png();
    REQUIRE(encoded);
    return std::move(*encoded);
}

// A 4 x 4 JPEG of one colour.
std::vector<std::uint8_t> jpeg(int red, int green, int blue) {
    QImage image(4, 4, QImage::Format_RGB888);
    image.fill(QColor(red, green, blue));
    QByteArray bytes;
    QBuffer buffer(&bytes);
    buffer.open(QIODevice::WriteOnly);
    REQUIRE(image.save(&buffer, "JPEG"));
    return {bytes.begin(), bytes.end()};
}

std::vector<std::uint8_t> read_file(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    REQUIRE(in);
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}

struct Nobody final : loa::bus::Authorize {
    std::expected<void, loa::bus::Denied> authorize(const loa::bus::SessionId&, const loa::bus::NodeKey&,
                                                    const loa::core::Draft&, const loa::core::Dom&) const override {
        return std::unexpected(loa::bus::Denied{"local only"});
    }
};

} // namespace

TEST_CASE("still frames land on the retained picture") {
    // (region, encoding, data): later regions overwrite.
    const std::array frames = {
        std::tuple{Rect{0, 0, 8, 8}, Encoding::Rgba, RgbaImage::filled(8, 8, {1, 1, 1, 255}).data},
        std::tuple{Rect{2, 1, 2, 2}, Encoding::Rgba, RgbaImage::filled(2, 2, {9, 9, 9, 255}).data},
        std::tuple{Rect{0, 4, 4, 4}, Encoding::Png, png(4, 4, {0, 200, 0, 255})},
        std::tuple{Rect{4, 4, 4, 4}, Encoding::Jpeg, jpeg(200, 0, 0)},
    };
    std::vector<std::uint8_t> picture(8 * 8 * 4, 0);
    adapters::StillDecoder still;
    std::uint64_t seq = 1;
    for (const auto& [region, encoding, data] : frames) {
        auto applied = still.apply(frame(seq++, region, encoding, data), picture, 8, 8);
        REQUIRE(applied);
        CHECK(*applied);
    }
    using Px = std::array<std::uint8_t, 4>;
    CHECK(pixel_at(picture, 2, 1) == Px{9, 9, 9, 255});
    CHECK(pixel_at(picture, 3, 2) == Px{9, 9, 9, 255});
    CHECK(pixel_at(picture, 1, 1) == Px{1, 1, 1, 255});
    CHECK(pixel_at(picture, 0, 7) == Px{0, 200, 0, 255});
    const auto [r, g, b, a] = pixel_at(picture, 5, 5);
    CHECK((r > 180 && g < 30 && b < 30 && a == 255));
    CHECK(std::ranges::equal(still.accepts(), std::array{Encoding::Rgba, Encoding::Png, Encoding::Jpeg}));
    auto outside = still.apply(frame(9, Rect{6, 6, 4, 4}, Encoding::Rgba, RgbaImage::filled(4, 4, {0, 0, 0, 0}).data),
                               picture, 8, 8);
    REQUIRE_FALSE(outside);
    CHECK(outside.error().message.find("outside") != std::string::npos);
}

TEST_CASE("documents open as images or rendered pages") {
    const auto sheet_bytes = read_file(fs::path(LOA_GOLDEN_DIR) / "docs" / "sample.xlsx");
    const Document xlsx{"a.xlsx", std::string(docs::mime(docs::Format::Xlsx)), sheet_bytes};
    const Document image{"a.png", "image/png", png(30, 20, {1, 2, 3, 255})};
    const Document unknown{"a.bin", "application/x-unknown", {}};
    const adapters::DocsRenderer renderer(docs::Fonts::none());
    const port::DocumentPalette palette{.page = {255, 255, 255, 255},
                                       .text = {0, 0, 0, 255},
                                       .rule = {128, 128, 128, 128},
                                       .grid = {217, 217, 217, 217},
                                       .header = {242, 242, 242, 242},
                                       .header_text = {64, 64, 64, 64}};

    auto opened = renderer.open(xlsx);
    REQUIRE(opened);
    auto* paged = std::get_if<port::opened_document::Paged>(&*opened);
    REQUIRE(paged);
    const auto& sheet = *paged->document;
    auto direct = docs::open_pages(docs::Format::Xlsx, sheet_bytes);
    REQUIRE(direct);
    CHECK(sheet.page_count() == (*direct)->page_count());
    CHECK(sheet.page_count() >= 1);
    CHECK(sheet.page_size(0));
    auto page = sheet.render(0, 1.25F, palette);
    REQUIRE(page);
    CHECK(page->width > 0);
    CHECK(page->height > 0);
    CHECK(page->data.size() == std::size_t{page->width} * page->height * 4);

    auto shown = renderer.open(image);
    REQUIRE(shown);
    auto* picture = std::get_if<port::opened_document::Image>(&*shown);
    REQUIRE(picture);
    CHECK(picture->width == 30);
    CHECK(picture->height == 20);

    auto refused = renderer.open(unknown);
    REQUIRE_FALSE(refused);
    CHECK(refused.error() == "no viewer for application/x-unknown");
}

TEST_CASE("the picker lists every docs format") {
    const auto extensions = adapters::DocsRenderer(docs::Fonts::none()).extensions();
    for (std::string_view ext : {"pdf", "xlsx", "xlsm", "docx", "pptx", "png", "jpg", "jpeg", "gif", "webp", "bmp"}) {
        CAPTURE(ext);
        CHECK(std::ranges::find(extensions, ext) != extensions.end());
    }
    CHECK(loa::view::surface::DOCUMENT_SERVER == docs::SERVER_NAME);
}

TEST_CASE("decoders put stills last") {
    const auto made = adapters::decoders();
    REQUIRE(made.still);
    CHECK(std::ranges::equal(made.still()->accepts(), loa::surfaces::STILL_ENCODINGS));
#ifdef LOA_SURFACES_REMOTE_VIDEO
    REQUIRE(made.extra);
    CHECK(std::ranges::equal((*made.extra)()->accepts(), std::array{Encoding::H264}));
#else
    CHECK_FALSE(made.extra);
#endif
}

TEST_CASE("library sessions list live and journaled sessions, newest first") {
    namespace bus = loa::bus;
    namespace attr = loa::core::attr;
    using loa::core::Draft;
    using loa::core::Value;
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto store = bus::Store::make(dir->path());
    REQUIRE(store);
    bus::NodeKey key;
    key.bytes.fill(1);
    auto library = std::make_shared<loa::session::Library>(std::move(*store), std::make_shared<bus::Bus>(), key,
                                                           std::make_shared<Nobody>());
    auto create = [&](const std::string& id, std::int64_t created) {
        auto live = library->create(id);
        REQUIRE(live);
        auto committed = live->writer->commit_at(
            Draft::insert(loa::core::NodeId::DOCUMENT, loa::core::ElementKind::Session,
                          {{std::string(attr::ID), Value(id)},
                           {std::string(attr::CREATED), Value(created)},
                           {std::string(attr::HARNESS), Value("omp")},
                           {std::string(attr::NODE), Value("me")},
                           {std::string(attr::TITLE), Value("title " + id)}}),
            created);
        if (!committed) FAIL("commit failed, alternative ", committed.error().index());
    };
    create("old", 1);
    create("new", 2);
    auto closed = library->close("old");
    REQUIRE(closed);
    CHECK(*closed);

    const adapters::LibrarySessions sessions(library);
    for (int pass = 0; pass < 2; ++pass) { // the second pass reads the closed session from the cache
        auto listed = sessions.sessions();
        REQUIRE(listed.size() == 2);
        CHECK(listed[0].facts.id == "new");
        CHECK(listed[1].facts.id == "old");
        CHECK(listed[1].title == "title old");
    }
    CHECK_FALSE(library->live("old"));
    auto writer = sessions.writer("old");
    REQUIRE(writer);
    CHECK(writer->is_writer());
    CHECK(library->live("old"));
    CHECK_FALSE(sessions.writer("missing"));
}
