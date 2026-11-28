#include <doctest/doctest.h>

#include <asio.hpp>

#include <cctype>
#include <csetjmp>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <sstream>

#include <gif_lib.h>
#include <jpeglib.h>
#include <webp/encode.h>

#include "loa/runtime/tempfile.hpp"
#include "loa/surfaces/docs/format.hpp"
#include "loa/surfaces/docs/images.hpp"
#include "loa/surfaces/docs/pdf.hpp"
#include "loa/surfaces/docs/raster.hpp"
#include "loa/surfaces/docs/server.hpp"

namespace docs = loa::surfaces::docs;
namespace json = loa::runtime::json;
using docs::Format;

namespace {

const std::filesystem::path GOLDEN = std::filesystem::path(LOA_GOLDEN_DIR) / "docs";

std::string read_text(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    REQUIRE(in);
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}

std::vector<std::uint8_t> read_bytes(const std::filesystem::path& path) {
    const std::string s = read_text(path);
    return {s.begin(), s.end()};
}

std::vector<std::uint8_t> bytes_of(std::string_view s) { return {s.begin(), s.end()}; }

std::string upper(std::string_view s) {
    std::string out(s);
    for (auto& c : out) c = static_cast<char>(std::toupper(static_cast<unsigned char>(c)));
    return out;
}

template <typename T>
T run(asio::io_context& io, loa::contracts::Awaitable<T> task) {
    std::optional<T> out;
    asio::co_spawn(io, std::move(task), [&](std::exception_ptr e, T value) {
        if (e) std::rethrow_exception(e);
        out.emplace(std::move(value));
    });
    io.restart();
    io.run();
    REQUIRE(out.has_value());
    return std::move(*out);
}

// --- Encoders for the 4 x 2 image fixture, left half red, right half blue. ---

constexpr std::uint32_t W = 4, H = 2;

std::vector<std::uint8_t> fixture_rgba() {
    std::vector<std::uint8_t> px;
    for (std::uint32_t y = 0; y < H; ++y)
        for (std::uint32_t x = 0; x < W; ++x) {
            const bool red = x < 2;
            px.insert(px.end(), {static_cast<std::uint8_t>(red ? 255 : 0), 0, static_cast<std::uint8_t>(red ? 0 : 255), 255});
        }
    return px;
}

std::vector<std::uint8_t> encode_gif() {
    std::vector<std::uint8_t> out;
    int error = 0;
    GifFileType* gif = EGifOpen(
        &out,
        [](GifFileType* g, const GifByteType* data, int len) {
            auto* o = static_cast<std::vector<std::uint8_t>*>(g->UserData);
            o->insert(o->end(), data, data + len);
            return len;
        },
        &error);
    REQUIRE(gif != nullptr);
    GifColorType colors[2] = {{255, 0, 0}, {0, 0, 255}};
    ColorMapObject* map = GifMakeMapObject(2, colors);
    EGifSetGifVersion(gif, true);
    REQUIRE(EGifPutScreenDesc(gif, W, H, 1, 0, map) == GIF_OK);
    REQUIRE(EGifPutImageDesc(gif, 0, 0, W, H, false, nullptr) == GIF_OK);
    for (std::uint32_t y = 0; y < H; ++y) {
        GifPixelType line[W] = {0, 0, 1, 1};
        REQUIRE(EGifPutLine(gif, line, W) == GIF_OK);
    }
    GifFreeMapObject(map);
    REQUIRE(EGifCloseFile(gif, &error) == GIF_OK);
    return out;
}

std::vector<std::uint8_t> encode_webp() {
    const auto px = fixture_rgba();
    std::uint8_t* data = nullptr;
    const std::size_t len = WebPEncodeLosslessRGBA(px.data(), W, H, W * 4, &data);
    REQUIRE(len > 0);
    std::vector<std::uint8_t> out(data, data + len);
    WebPFree(data);
    return out;
}

std::vector<std::uint8_t> encode_bmp() {
    // 24 bit BI_RGB, bottom-up rows padded to 4 bytes.
    const std::uint32_t row = (W * 3 + 3) & ~3U, image = row * H, size = 54 + image;
    std::vector<std::uint8_t> out(size, 0);
    const auto put = [&](std::size_t at, std::uint32_t v, int n) {
        for (int i = 0; i < n; ++i) out[at + i] = static_cast<std::uint8_t>(v >> (8 * i));
    };
    out[0] = 'B', out[1] = 'M';
    put(2, size, 4), put(10, 54, 4), put(14, 40, 4), put(18, W, 4), put(22, H, 4), put(26, 1, 2), put(28, 24, 2), put(34, image, 4);
    const auto px = fixture_rgba();
    for (std::uint32_t y = 0; y < H; ++y)
        for (std::uint32_t x = 0; x < W; ++x) {
            const std::uint8_t* s = px.data() + (static_cast<std::size_t>(H - 1 - y) * W + x) * 4;
            std::uint8_t* d = out.data() + 54 + y * row + x * 3;
            d[0] = s[2], d[1] = s[1], d[2] = s[0];
        }
    return out;
}

std::vector<std::uint8_t> encode_jpeg() {
    jpeg_compress_struct cinfo{};
    jpeg_error_mgr jerr{};
    cinfo.err = jpeg_std_error(&jerr);
    jpeg_create_compress(&cinfo);
    unsigned char* buffer = nullptr;
    unsigned long len = 0;
    jpeg_mem_dest(&cinfo, &buffer, &len);
    cinfo.image_width = W;
    cinfo.image_height = H;
    cinfo.input_components = 3;
    cinfo.in_color_space = JCS_RGB;
    jpeg_set_defaults(&cinfo);
    jpeg_set_quality(&cinfo, 90, TRUE);
    jpeg_start_compress(&cinfo, TRUE);
    const auto px = fixture_rgba();
    for (std::uint32_t y = 0; y < H; ++y) {
        std::uint8_t line[W * 3];
        for (std::uint32_t x = 0; x < W; ++x)
            for (int c = 0; c < 3; ++c) line[x * 3 + c] = px[(y * W + x) * 4 + c];
        JSAMPROW row = line;
        jpeg_write_scanlines(&cinfo, &row, 1);
    }
    jpeg_finish_compress(&cinfo);
    jpeg_destroy_compress(&cinfo);
    std::vector<std::uint8_t> out(buffer, buffer + len);
    std::free(buffer);
    return out;
}

} // namespace

TEST_CASE("formats are detected by extension, mime and magic") {
    auto zip = bytes_of(std::string_view("PK\x03\x04....", 8));
    const auto part = bytes_of("word/document.xml");
    zip.insert(zip.end(), part.begin(), part.end());
    const auto png = loa::surfaces::encode_png(1, 1, std::vector<std::uint8_t>(4, 0));
    REQUIRE(png);
    struct Case {
        const char* path;
        std::vector<std::uint8_t> bytes;
        std::optional<Format> want;
    };
    const Case cases[] = {
        {"upload", zip, Format::Docx},
        {"a.PDF", {}, Format::Pdf},
        {"b.xlsm", {}, Format::Xlsx},
        {"c.jpeg", {}, Format::Jpeg},
        {"noext", bytes_of("%PDF-1.7"), Format::Pdf},
        {"noext", bytes_of(std::string_view("PK\x03\x04 other zip", 14)), std::nullopt},
        {"noext", *png, Format::Png},
        {"notes.txt", bytes_of("plain"), std::nullopt},
    };
    for (const auto& c : cases) {
        CAPTURE(c.path);
        CHECK(docs::detect(c.path, c.bytes) == c.want);
    }
    for (const auto& spec : docs::formats()) {
        CHECK(docs::from_mime(upper(spec.mime)) == spec.format);
        CHECK(docs::to_string(spec.format) == spec.name);
    }
    CHECK(docs::is_image(Format::Gif));
    CHECK_FALSE(docs::is_image(Format::Pptx));
}

TEST_CASE("every image format decodes, scales and reports corruption") {
    const auto png = loa::surfaces::encode_png(W, H, fixture_rgba());
    REQUIRE(png);
    const std::pair<Format, std::vector<std::uint8_t>> fixtures[] = {
        {Format::Png, *png}, {Format::Gif, encode_gif()}, {Format::Webp, encode_webp()}, {Format::Bmp, encode_bmp()}, {Format::Jpeg, encode_jpeg()},
    };
    for (const auto& [format, bytes] : fixtures) {
        CAPTURE(docs::to_string(format));
        CHECK(docs::sniff(bytes) == format);
        auto doc = docs::images::open(format, bytes);
        REQUIRE(doc);
        auto page = (*doc)->render_page(0, 2.0F, docs::Fonts::none());
        REQUIRE(page);
        CHECK(page->width == 8);
        CHECK(page->height == 4);
        const auto px = page->pixel(0, 0);
        REQUIRE(px);
        CHECK((*px)[0] > 150);
        CHECK((*px)[2] < 120);
        CHECK((*px)[3] == 255);
        auto broken = docs::images::open(format, std::span(bytes).first(20));
        REQUIRE_FALSE(broken);
        CHECK(broken.error().kind() == docs::DocsError::Kind::Invalid);
    }
    CHECK(docs::images::open(Format::Pdf, {}).error().kind() == docs::DocsError::Kind::Unsupported);
}

TEST_CASE("DataQuery JSON round-trips the serde golden") {
    std::istringstream lines(read_text(GOLDEN / "queries.jsonl"));
    std::vector<docs::DataQuery> queries;
    for (std::string line; std::getline(lines, line);) {
        auto value = json::parse(line);
        REQUIRE(value);
        auto query = docs::DataQuery::from_json(*value);
        REQUIRE(query);
        CHECK(json::dump(query->to_json()) == line);
        queries.push_back(*query);
    }
    REQUIRE(queries.size() == 8);
    CHECK(queries[2] == docs::DataQuery{docs::query::Text{2}});
    CHECK(queries[5] == docs::DataQuery{docs::query::Cells{"Data", "C3"}});
    // `#[serde(default)]` fields may be missing.
    auto missing = docs::DataQuery::from_json(*json::parse(R"({"kind":"cells","range":"A1"})"));
    REQUIRE(missing);
    CHECK(*missing == docs::DataQuery{docs::query::Cells{std::nullopt, "A1"}});
    CHECK_FALSE(docs::DataQuery::from_json(*json::parse(R"({"kind":"cells"})")));
    CHECK_FALSE(docs::DataQuery::from_json(*json::parse(R"({"kind":"pages"})")));
}

TEST_CASE("a PDF matches the Rust page sizes, render sizes, text and pixels") {
    auto doc = docs::PdfDoc::parse(read_bytes(GOLDEN / "sample.pdf"));
    REQUIRE(doc);
    std::istringstream sizes(read_text(GOLDEN / "sample.sizes.txt"));
    std::size_t pages = 0;
    for (std::size_t index = 0; sizes >> index;) {
        float width = 0, height = 0, scale = 0;
        std::uint32_t rw = 0, rh = 0;
        sizes >> width >> height >> scale >> rw >> rh;
        CAPTURE(index);
        auto size = (*doc)->page_size(index);
        REQUIRE(size);
        CHECK(size->width == width);
        CHECK(size->height == height);
        auto image = (*doc)->render_page(index, scale, docs::Fonts::none());
        REQUIRE(image);
        CHECK(image->width == rw);
        CHECK(image->height == rh);
        ++pages;
    }
    CHECK((*doc)->page_count() == pages);

    auto text = (*doc)->query(docs::DataQuery{docs::query::Text{}});
    REQUIRE(text);
    REQUIRE(*text);
    CHECK(json::dump(**text) == read_text(GOLDEN / "text.json"));

    auto image = (*doc)->render_page(0, 2.0F, docs::Fonts::none());
    REQUIRE(image);
    CHECK(image->width == 1224);
    CHECK(image->height == 1584);
    // PDF y 100..150 is device y (792 - 150)..(792 - 100), doubled.
    CHECK(image->pixel(250, 1334) == loa::surfaces::Pixel{0, 0, 255, 255});
    CHECK(image->pixel(350, 1334) == loa::surfaces::Pixel{255, 255, 255, 255});
    std::size_t ink = 0;
    for (std::uint32_t x = 144; x < 360; ++x)
        for (std::uint32_t y = 144; y < 184; ++y) ink += (*image->pixel(x, y))[0] < 128;
    CHECK(ink > 50);
    // Rotating 90 degrees clockwise moves the bottom left corner to the top left.
    auto rotated = (*doc)->render_page(2, 1.0F, docs::Fonts::none());
    REQUIRE(rotated);
    CHECK(rotated->width == 792);
    CHECK(rotated->height == 612);
    CHECK(rotated->pixel(25, 25) == loa::surfaces::Pixel{255, 0, 0, 255});

    auto out_of_range = (*doc)->query(docs::DataQuery{docs::query::Text{3 + 1}});
    REQUIRE(out_of_range);
    REQUIRE_FALSE(*out_of_range);
    CHECK(out_of_range->error().kind() == docs::DocsError::Kind::PageRange);
    CHECK(out_of_range->error().message() == "page 4 is out of range, the document has 4");
    CHECK((*doc)->render_page(0, 200.0F, docs::Fonts::none()).error().kind() == docs::DocsError::Kind::Request);
    CHECK((*doc)->render_page(0, 0.0F, docs::Fonts::none()).error().kind() == docs::DocsError::Kind::Request);
    CHECK_FALSE((*doc)->query(docs::DataQuery{docs::query::Sheets{}}));
    auto bad = docs::PdfDoc::parse(bytes_of("not a pdf"));
    REQUIRE_FALSE(bad);
    CHECK(bad.error().kind() == docs::DocsError::Kind::Invalid);
    CHECK(bad.error().format() == Format::Pdf);
}

TEST_CASE("DocsServer opens a file, serves info and closes it") {
    namespace cs = loa::contracts::surface;
    asio::io_context io;
    docs::DocsServer server(docs::Fonts::none());

    // `info.json` was written with the PDF opened by its relative path.
    const auto cwd = std::filesystem::current_path();
    std::filesystem::current_path(GOLDEN);
    auto opened = run(io, server.open_file("sample.pdf"));
    std::filesystem::current_path(cwd);
    REQUIRE(opened);
    auto surface = *opened;
    CHECK(surface->uri().as_str() == "surface://docs/1");
    auto info = run(io, surface->data(docs::DataQuery{docs::query::Info{}}));
    REQUIRE(info);
    CHECK(json::dump(*info) == read_text(GOLDEN / "info.json"));
    auto sheets = surface->answer(docs::DataQuery{docs::query::Sheets{}});
    REQUIRE_FALSE(sheets);
    CHECK(sheets.error().message() == "PDF files have no sheets");

    const cs::SurfaceInfo si = surface->info();
    CHECK(si.width == 612);
    CHECK(si.height == 792);
    CHECK(si.mode == cs::SurfaceMode::Document);
    CHECK(si.mime == std::optional<std::string>("application/pdf"));
    CHECK(surface->frames().error().kind() == cs::SurfaceError::Kind::Mode);
    auto document = run(io, surface->document());
    REQUIRE(document);
    CHECK(document->name == "sample.pdf");
    CHECK(document->data == read_bytes(GOLDEN / "sample.pdf"));
    auto shot = run(io, surface->screenshot());
    REQUIRE(shot);
    CHECK(loa::surfaces::png_size(*shot) == std::pair<std::uint32_t, std::uint32_t>{918, 1188});

    CHECK(server.surface(surface->uri()));
    server.close(surface->uri());
    auto gone = server.surface(surface->uri());
    REQUIRE_FALSE(gone);
    CHECK(gone.error().to_surface_error().kind() == cs::SurfaceError::Kind::NotFound);

    // A broken or unknown file fails at open.
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto write = [&](const char* name, std::string_view content) {
        std::ofstream(dir->path() / name, std::ios::binary) << content;
        return dir->path() / name;
    };
    auto broken = run(io, server.open_file(write("broken.pdf", "%PDF-1.4 truncated")));
    REQUIRE_FALSE(broken);
    CHECK(broken.error().kind() == docs::DocsError::Kind::Invalid);
    auto unknown = run(io, server.open_file(write("notes.txt", "plain")));
    REQUIRE_FALSE(unknown);
    CHECK(unknown.error().kind() == docs::DocsError::Kind::Unsupported);
    auto missing = run(io, server.open_file(dir->path() / "missing.pdf"));
    REQUIRE_FALSE(missing);
    CHECK(missing.error().kind() == docs::DocsError::Kind::Io);
    CHECK(missing.error().to_surface_error().kind() == cs::SurfaceError::Kind::Backend);

    // A second open gets the next id; an image screenshot is its PNG file.
    const auto png = loa::surfaces::encode_png(W, H, fixture_rgba());
    std::ofstream(dir->path() / "pic.png", std::ios::binary).write(reinterpret_cast<const char*>(png->data()), static_cast<std::streamsize>(png->size()));
    auto pic = run(io, server.open_file(dir->path() / "pic.png"));
    REQUIRE(pic);
    CHECK((*pic)->uri().as_str() == "surface://docs/2");
    CHECK(*run(io, (*pic)->screenshot()) == *png);
}

TEST_CASE("raster text without a font is boxes and wraps by words") {
    const auto fonts = docs::Fonts::none();
    CHECK(fonts.text_width("abcd", 10.0F, false) == doctest::Approx(24.0));
    const auto lines = fonts.wrap("aa bb cc\ndd", 10.0F, false, 40.0F);
    REQUIRE(lines.size() == 3);
    CHECK(lines[0] == "aa bb");
    CHECK(lines[1] == "cc");
    CHECK(lines[2] == "dd");
    docs::Canvas canvas(20, 10, docs::Color{{255, 255, 255, 255}});
    canvas.fill_rect(2, 2, 3, 3, docs::Color{{0, 0, 0, 255}});
    const float advance = canvas.draw_text(fonts, docs::RenderOpts{}.text(10.0F), 10, 0, "x");
    CHECK(advance == doctest::Approx(6.0));
    const auto rgba = std::move(canvas).into_rgba();
    CHECK(rgba.pixel(3, 3) == loa::surfaces::Pixel{0, 0, 0, 255});
    CHECK(rgba.pixel(0, 0) == loa::surfaces::Pixel{255, 255, 255, 255});

    const auto system = docs::Fonts::system();
    if (system.has_regular()) {
        docs::Canvas text(60, 20, docs::Color{{255, 255, 255, 255}});
        CHECK(text.draw_text(system, docs::RenderOpts{}.text(14.0F), 2, 2, "Hi") > 0.0F);
        std::size_t ink = 0;
        const auto out = std::move(text).into_rgba();
        for (std::size_t i = 0; i < out.data.size(); i += 4) ink += out.data[i] < 128;
        CHECK(ink > 5);
    }
}
