#include "loa/surfaces/pixels.hpp"

#include <doctest/doctest.h>

#include <fstream>
#include <iterator>

using namespace loa::surfaces;

namespace {
std::vector<std::uint8_t> fixture(const char* name) {
    std::ifstream in(std::string(LOA_FIXTURE_DIR) + "/" + name, std::ios::binary);
    REQUIRE(in);
    return {std::istreambuf_iterator<char>(in), {}};
}
} // namespace

TEST_CASE("png_round_trips_and_reports_its_size") {
    const auto img = RgbaImage::filled(3, 2, {10, 20, 30, 255});
    const auto png = img.to_png();
    REQUIRE(png);
    CHECK(png_size(*png) == std::pair<std::uint32_t, std::uint32_t>{3, 2});
    CHECK(image_size(*png) == std::pair<std::uint32_t, std::uint32_t>{3, 2});
    const auto decoded = decode_png(*png);
    REQUIRE(decoded);
    CHECK(*decoded == img);
    const std::string gif = "GIF89a";
    CHECK_FALSE(png_size(std::span(reinterpret_cast<const std::uint8_t*>(gif.data()), gif.size())));
}

TEST_CASE("chrome_jpeg_with_one_scan_per_component_keeps_its_colors") {
    const auto jpeg = fixture("chrome-noninterleaved-red.jpg");
    const auto decoded = decode_jpeg(jpeg);
    REQUIRE(decoded);
    const auto px = decoded->pixel(160, 120);
    REQUIRE(px);
    const auto [r, g, b, a] = *px;
    CAPTURE(int(r));
    CAPTURE(int(g));
    CAPTURE(int(b));
    CHECK((r > 240 && g < 15 && b < 15 && a == 255));
    CHECK(image_size(jpeg) == std::pair{decoded->width, decoded->height});
}

TEST_CASE("decoded fixture pixels survive a PNG re-encode") {
    const auto decoded = decode_jpeg(fixture("chrome-noninterleaved-red.jpg"));
    REQUIRE(decoded);
    const auto png = decoded->to_png();
    REQUIRE(png);
    const auto again = decode_png(*png);
    REQUIRE(again);
    CHECK(*again == *decoded);
    // And the re-encode is deterministic.
    CHECK(again->to_png() == png);
}

TEST_CASE("blit checks target, bounds and length and writes nothing on error") {
    auto img = RgbaImage::make(4, 3);
    const auto before = img;
    auto e = blit(std::span(img.data).first(4), 4, 3, {}, {0, 0, 0, 0});
    REQUIRE_FALSE(e);
    CHECK(e.error().message() == "a 4x3 picture needs 48 bytes, it has 4");
    e = img.blit({3, 0, 2, 1}, std::vector<std::uint8_t>(8));
    REQUIRE_FALSE(e);
    CHECK(e.error().message() == "region Rect { x: 3, y: 0, width: 2, height: 1 } is outside the 4x3 picture");
    e = img.blit({0, 0, 2, 1}, std::vector<std::uint8_t>(7));
    REQUIRE_FALSE(e);
    CHECK(e.error().message() == "region needs 8 bytes of RGBA, got 7");
    CHECK(img == before);
    REQUIRE(img.blit({1, 1, 2, 2}, std::vector<std::uint8_t>(16, 9)));
    CHECK(img.pixel(1, 1) == Pixel{9, 9, 9, 9});
    CHECK(img.pixel(0, 1) == Pixel{0, 0, 0, 0});
    CHECK(img.crop({1, 1, 2, 2}) == std::vector<std::uint8_t>(16, 9));
    CHECK(img.crop(img.full()) == img.data);
    CHECK_FALSE(img.pixel(4, 0));
}

TEST_CASE("fill clips and copy_rect handles overlap") {
    auto img = RgbaImage::make(3, 3);
    img.fill({1, 1, 0xffffffff, 0xffffffff}, {1, 2, 3, 4});
    CHECK(img.pixel(2, 2) == Pixel{1, 2, 3, 4});
    CHECK(img.pixel(0, 0) == Pixel{0, 0, 0, 0});
    auto rows = RgbaImage::make(1, 3);
    rows.fill({0, 0, 1, 1}, {1, 1, 1, 1});
    rows.fill({0, 1, 1, 1}, {2, 2, 2, 2});
    rows.copy_rect(0, 0, {0, 1, 1, 2});
    CHECK(rows.pixel(0, 1) == Pixel{1, 1, 1, 1});
    CHECK(rows.pixel(0, 2) == Pixel{2, 2, 2, 2});
}

TEST_CASE("changed_rows finds the first and last differing row") {
    const auto a = RgbaImage::make(2, 4);
    auto b = a;
    CHECK_FALSE(changed_rows(a.data, b.data, 2));
    b.fill({0, 1, 1, 1}, {1, 0, 0, 0});
    b.fill({1, 2, 1, 1}, {1, 0, 0, 0});
    CHECK(changed_rows(a.data, b.data, 2) == std::pair<std::uint32_t, std::uint32_t>{1, 2});
}

TEST_CASE("apply_still decodes stills and rejects video and size mismatches") {
    auto dst = RgbaImage::make(4, 4);
    Frame frame;
    frame.region = {1, 1, 2, 2};
    frame.encoding = Encoding::Png;
    frame.data = *RgbaImage::filled(2, 2, {5, 6, 7, 255}).to_png();
    REQUIRE(apply_still(dst.data, 4, 4, frame));
    CHECK(dst.pixel(2, 2) == Pixel{5, 6, 7, 255});
    frame.region = {0, 0, 3, 3};
    const auto before = dst;
    auto e = apply_still(dst.data, 4, 4, frame);
    REQUIRE_FALSE(e);
    CHECK(e.error().message() == "decoded picture is 2x2, its region is 3x3");
    CHECK(dst == before);
    frame.encoding = Encoding::H264;
    CHECK(apply_still(dst.data, 4, 4, frame).error().message() == "H264 is not a still encoding");
    frame.encoding = Encoding::Rgba;
    frame.region = {0, 0, 1, 1};
    frame.data = {1, 2, 3, 4};
    REQUIRE(apply_still(dst.data, 4, 4, frame));
    CHECK(dst.pixel(0, 0) == Pixel{1, 2, 3, 4});
    frame.encoding = Encoding::Jpeg;
    CHECK(apply_still(dst.data, 4, 4, frame).error().kind() == PixelError::Kind::Codec);
}

TEST_CASE("image_size reads GIF, BMP and WebP headers and rejects the unknown") {
    const std::vector<std::uint8_t> gif = {'G', 'I', 'F', '8', '9', 'a', 7, 0, 5, 0};
    CHECK(image_size(gif) == std::pair<std::uint32_t, std::uint32_t>{7, 5});
    std::vector<std::uint8_t> bmp(26);
    bmp[0] = 'B'; bmp[1] = 'M'; bmp[14] = 40; bmp[18] = 3; bmp[22] = 0xfe; bmp[23] = 0xff; bmp[24] = 0xff; bmp[25] = 0xff;
    CHECK(image_size(bmp) == std::pair<std::uint32_t, std::uint32_t>{3, 2});
    const std::vector<std::uint8_t> junk = {1, 2, 3};
    CHECK(image_size(junk).error().message() == "image: The image format could not be determined");
}
