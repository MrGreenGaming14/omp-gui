#pragma once

// A tiny reference RFB encoder (pictures to wire rectangles) and a scripted RFB
// server. Port of `testkit::rfb` in `loa-surfaces/src/testkit.rs`. Test-only:
// includes Asio and throws `std::runtime_error` where Rust panics.

#include <asio/awaitable.hpp>
#include <asio/ip/tcp.hpp>

#include <cstdint>
#include <memory>
#include <optional>
#include <span>
#include <string_view>
#include <vector>

#include "loa/surfaces/pixels.hpp"

namespace loa::surfaces::testkit::rfb {

// The server side of one RFB connection.
class FakeRfb {
  public:
    explicit FakeRfb(asio::ip::tcp::socket socket) : socket_(std::move(socket)) {}

    // The next connection within `PATIENCE`.
    [[nodiscard]] static asio::awaitable<FakeRfb> accept(asio::ip::tcp::acceptor& listener);

    asio::awaitable<void> send(std::span<const std::uint8_t> bytes);
    // Exactly `n` bytes within `PATIENCE`.
    [[nodiscard]] asio::awaitable<std::vector<std::uint8_t>> read(std::size_t n);
    asio::awaitable<void> expect(std::span<const std::uint8_t> want);
    // Expects an incremental FramebufferUpdateRequest for the whole framebuffer.
    asio::awaitable<void> expect_request(std::uint16_t width, std::uint16_t height);
    // RFB 3.8 up to the client's setup, with VNC Authentication when
    // `password` is set; false when the client's VNC response was wrong.
    [[nodiscard]] asio::awaitable<bool> handshake(std::optional<std::string_view> password, std::uint16_t width,
                                                  std::uint16_t height);
    // ServerInit, then the client's SetPixelFormat, SetEncodings and full update request.
    asio::awaitable<void> server_init(std::uint16_t width, std::uint16_t height, std::string_view name);

    asio::ip::tcp::socket& socket() { return socket_; }

  private:
    asio::ip::tcp::socket socket_;
};

// ZRLE tile subencodings the encoder picks (single colour tiles are always solid).
enum class Zrle : std::uint8_t { Raw, Packed, PlainRle, PaletteRle };

// One zlib stream (flate2 `Compress` at the default level, with zlib header).
class Deflater {
  public:
    Deflater();
    ~Deflater();
    Deflater(const Deflater&) = delete;
    Deflater& operator=(const Deflater&) = delete;
    // Compresses `input` with a sync flush.
    [[nodiscard]] std::vector<std::uint8_t> sync(std::span<const std::uint8_t> input);

  private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

// A wire pixel `R G B X` with a junk padding byte.
[[nodiscard]] Pixel pixel(std::span<const std::uint8_t> p);
[[nodiscard]] std::vector<std::uint8_t> raw(const RgbaImage& img);
// The first pixel as background, every other pixel a 1x1 subrectangle.
[[nodiscard]] std::vector<std::uint8_t> rre(const RgbaImage& img);
// Solid tiles send only a changed background; two-colour tiles use foreground
// subrects, others alternate between raw and coloured subrects.
[[nodiscard]] std::vector<std::uint8_t> hextile(const RgbaImage& img);
// 64x64 tiles with subencoding `mode`, compressed on the connection's stream `z`.
[[nodiscard]] std::vector<std::uint8_t> zrle(const RgbaImage& img, Zrle mode, Deflater& z);
// A rectangle header and its payload.
[[nodiscard]] std::vector<std::uint8_t> rect(std::uint16_t x, std::uint16_t y, std::uint16_t w, std::uint16_t h,
                                             std::int32_t encoding, std::span<const std::uint8_t> payload);
// A FramebufferUpdate of `rects`.
[[nodiscard]] std::vector<std::uint8_t> update(const std::vector<std::vector<std::uint8_t>>& rects);

} // namespace loa::surfaces::testkit::rfb
