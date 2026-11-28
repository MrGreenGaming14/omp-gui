#include "loa/surfaces/vnc/testkit.hpp"

#include <zlib.h>

#include <asio.hpp>
#include <asio/experimental/awaitable_operators.hpp>

#include <algorithm>
#include <stdexcept>

#include "loa/surfaces/testkit.hpp"
#include "loa/surfaces/vnc/decode.hpp"
#include "loa/surfaces/vnc/proto.hpp"

namespace loa::surfaces::testkit::rfb {

namespace {

using Bytes = std::vector<std::uint8_t>;
using Px = std::span<const std::uint8_t>;

void be16(Bytes& out, std::uint16_t v) {
    out.push_back(std::uint8_t(v >> 8U));
    out.push_back(std::uint8_t(v));
}
void be32(Bytes& out, std::uint32_t v) {
    for (int s = 24; s >= 0; s -= 8) out.push_back(std::uint8_t(v >> unsigned(s)));
}
void append(Bytes& out, std::span<const std::uint8_t> b) { out.insert(out.end(), b.begin(), b.end()); }

std::vector<Px> pixels(const RgbaImage& img) {
    std::vector<Px> out;
    for (std::size_t i = 0; i + 4 <= img.data.size(); i += 4) out.emplace_back(img.data.data() + i, 4);
    return out;
}

bool same(Px a, Px b) { return std::ranges::equal(a, b); }

RgbaImage tile(const RgbaImage& img, std::uint32_t x0, std::uint32_t y0, std::uint32_t size) {
    const std::uint32_t w = std::min(size, img.width - x0);
    const std::uint32_t h = std::min(size, img.height - y0);
    return RgbaImage{w, h, img.crop(Rect{x0, y0, w, h})};
}

std::vector<RgbaImage> tiles(const RgbaImage& img, std::uint32_t size) {
    std::vector<RgbaImage> out;
    for (std::uint32_t y = 0; y < img.height; y += size) {
        for (std::uint32_t x = 0; x < img.width; x += size) out.push_back(tile(img, x, y, size));
    }
    return out;
}

std::vector<Px> colours(const std::vector<Px>& ps) {
    std::vector<Px> seen;
    for (const Px p : ps) {
        if (std::ranges::none_of(seen, [&](Px q) { return same(p, q); })) seen.push_back(p);
    }
    return seen;
}

void run(Bytes& out, std::size_t len) {
    out.insert(out.end(), (len - 1) / 255, 255);
    out.push_back(std::uint8_t((len - 1) % 255));
}

asio::awaitable<void> within_patience(asio::awaitable<void> op) {
    using namespace asio::experimental::awaitable_operators;
    asio::steady_timer timer(co_await asio::this_coro::executor, PATIENCE);
    auto result = co_await (std::move(op) || timer.async_wait(asio::use_awaitable));
    if (result.index() == 1) throw std::runtime_error("RFB fake timed out");
}

} // namespace

asio::awaitable<FakeRfb> FakeRfb::accept(asio::ip::tcp::acceptor& listener) {
    std::optional<asio::ip::tcp::socket> socket;
    co_await within_patience([](asio::ip::tcp::acceptor& l, std::optional<asio::ip::tcp::socket>& s) -> asio::awaitable<void> {
        s.emplace(co_await l.async_accept(asio::use_awaitable));
    }(listener, socket));
    co_return FakeRfb(std::move(*socket));
}

asio::awaitable<void> FakeRfb::send(std::span<const std::uint8_t> bytes) {
    co_await asio::async_write(socket_, asio::buffer(bytes.data(), bytes.size()), asio::use_awaitable);
}

asio::awaitable<std::vector<std::uint8_t>> FakeRfb::read(std::size_t n) {
    Bytes buf(n);
    co_await within_patience([](asio::ip::tcp::socket& s, Bytes& b) -> asio::awaitable<void> {
        co_await asio::async_read(s, asio::buffer(b), asio::use_awaitable);
    }(socket_, buf));
    co_return buf;
}

asio::awaitable<void> FakeRfb::expect(std::span<const std::uint8_t> want) {
    const Bytes got = co_await read(want.size());
    if (!std::ranges::equal(got, want)) throw std::runtime_error("RFB fake read unexpected bytes");
}

asio::awaitable<void> FakeRfb::expect_request(std::uint16_t width, std::uint16_t height) {
    co_await expect(vnc::Msg::update_request(true, width, height).bytes());
}

asio::awaitable<bool> FakeRfb::handshake(std::optional<std::string_view> password, std::uint16_t width,
                                         std::uint16_t height) {
    const std::string_view banner = "RFB 003.008\n";
    const Bytes banner_bytes(banner.begin(), banner.end());
    co_await send(banner_bytes);
    co_await expect(banner_bytes);
    if (password) {
        co_await send(Bytes{2, 16, 2});
        co_await expect(Bytes{2});
        std::array<std::uint8_t, 16> challenge{};
        for (std::size_t i = 0; i < challenge.size(); ++i) challenge[i] = std::uint8_t(i * 37);
        co_await send(challenge);
        const Bytes got = co_await read(16);
        const auto want = vnc::vnc_response({reinterpret_cast<const std::uint8_t*>(password->data()), password->size()}, challenge);
        if (!std::ranges::equal(got, want)) {
            Bytes failed;
            be32(failed, 1);
            be32(failed, 12);
            for (const char c : std::string_view("bad password")) failed.push_back(std::uint8_t(c));
            co_await send(failed);
            co_return false;
        }
    } else {
        co_await send(Bytes{1, 1});
        co_await expect(Bytes{1});
    }
    co_await send(Bytes{0, 0, 0, 0});
    co_await expect(Bytes{1}); // ClientInit, shared
    co_await server_init(width, height, "fake desk");
    co_return true;
}

asio::awaitable<void> FakeRfb::server_init(std::uint16_t width, std::uint16_t height, std::string_view name) {
    // Server native format: 32 bpp big endian, blue in the low byte.
    Bytes init;
    be16(init, width);
    be16(init, height);
    append(init, Bytes{32, 24, 1, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0});
    be32(init, std::uint32_t(name.size()));
    for (const char c : name) init.push_back(std::uint8_t(c));
    co_await send(init);
    const std::array<std::int32_t, 7> encodings{1, 16, 5, 2, 0, -239, -223};
    Bytes setup;
    vnc::Msg::set_pixel_format().write(setup);
    vnc::Msg::set_encodings(encodings).write(setup);
    vnc::Msg::update_request(false, width, height).write(setup);
    co_await expect(setup);
}

struct Deflater::Impl {
    z_stream z{};
};

Deflater::Deflater() : impl_(std::make_unique<Impl>()) { deflateInit(&impl_->z, Z_DEFAULT_COMPRESSION); }
Deflater::~Deflater() { deflateEnd(&impl_->z); }

std::vector<std::uint8_t> Deflater::sync(std::span<const std::uint8_t> input) {
    Bytes out(input.size() * 2 + 64);
    auto& z = impl_->z;
    z.next_in = const_cast<Bytef*>(input.data());
    z.avail_in = uInt(input.size());
    z.next_out = out.data();
    z.avail_out = uInt(out.size());
    if (deflate(&z, Z_SYNC_FLUSH) != Z_OK || z.avail_out == 0 || z.avail_in != 0) {
        throw std::runtime_error("compressed output was cut short");
    }
    out.resize(out.size() - z.avail_out);
    return out;
}

Pixel pixel(std::span<const std::uint8_t> p) { return {p[0], p[1], p[2], 0x77}; }

std::vector<std::uint8_t> raw(const RgbaImage& img) {
    Bytes out;
    for (const Px p : pixels(img)) append(out, pixel(p));
    return out;
}

std::vector<std::uint8_t> rre(const RgbaImage& img) {
    const auto ps = pixels(img);
    const Px bg = ps.at(0);
    std::vector<std::pair<std::size_t, Px>> subs;
    for (std::size_t i = 0; i < ps.size(); ++i) {
        if (!same(ps[i], bg)) subs.emplace_back(i, ps[i]);
    }
    Bytes out;
    be32(out, std::uint32_t(subs.size()));
    append(out, pixel(bg));
    for (const auto& [i, p] : subs) {
        append(out, pixel(p));
        for (const std::uint32_t f : {std::uint32_t(i) % img.width, std::uint32_t(i) / img.width, 1U, 1U}) {
            be16(out, std::uint16_t(f));
        }
    }
    return out;
}

std::vector<std::uint8_t> hextile(const RgbaImage& img) {
    Bytes out;
    std::optional<Bytes> bg;
    std::size_t n = 0;
    for (const RgbaImage& t : tiles(img, 16)) {
        const auto ps = pixels(t);
        const auto c = colours(ps);
        std::vector<std::pair<std::size_t, Px>> subs;
        for (std::size_t i = 0; i < ps.size(); ++i) {
            if (!same(ps[i], c[0])) subs.emplace_back(i, ps[i]);
        }
        const std::size_t index = n++;
        if (c.size() > 2 && (index % 2 == 0 || subs.size() > 255)) {
            out.push_back(1);
            append(out, raw(t));
            bg.reset();
            continue;
        }
        std::uint8_t flags = bg && same(*bg, c[0]) ? 0 : 2;
        flags |= c.size() == 1 ? 0 : c.size() == 2 ? (4 | 8) : (8 | 16);
        out.push_back(flags);
        if ((flags & 2) != 0) append(out, pixel(c[0]));
        if ((flags & 4) != 0) append(out, pixel(c[1]));
        if ((flags & 8) != 0) {
            out.push_back(std::uint8_t(subs.size()));
            for (const auto& [i, p] : subs) {
                if ((flags & 16) != 0) append(out, pixel(p));
                out.push_back(std::uint8_t(((std::uint32_t(i) % t.width) << 4U) | (std::uint32_t(i) / t.width)));
                out.push_back(0);
            }
        }
        bg = Bytes(c[0].begin(), c[0].end());
    }
    return out;
}

std::vector<std::uint8_t> zrle(const RgbaImage& img, Zrle mode, Deflater& z) {
    Bytes tiles_out;
    for (const RgbaImage& t : tiles(img, 64)) {
        const auto ps = pixels(t);
        const auto c = colours(ps);
        const auto index = [&](Px p) {
            return std::size_t(std::ranges::find_if(c, [&](Px q) { return same(p, q); }) - c.begin());
        };
        const auto cpixels = [&](const std::vector<Px>& list) {
            for (const Px p : list) append(tiles_out, p.first(3));
        };
        std::vector<std::pair<Px, std::size_t>> runs;
        for (const Px p : ps) {
            if (!runs.empty() && same(runs.back().first, p)) {
                ++runs.back().second;
            } else {
                runs.emplace_back(p, 1);
            }
        }
        if (c.size() == 1) {
            tiles_out.push_back(1);
            cpixels(c);
        } else if (mode == Zrle::Packed && c.size() <= 16) {
            tiles_out.push_back(std::uint8_t(c.size()));
            cpixels(c);
            const std::size_t bits = c.size() == 2 ? 1 : c.size() <= 4 ? 2 : 4;
            for (std::size_t y = 0; y < t.height; ++y) {
                Bytes packed((t.width * bits + 7) / 8, 0);
                for (std::size_t x = 0; x < t.width; ++x) {
                    packed[x * bits / 8] |= std::uint8_t(index(ps[y * t.width + x]) << (8 - bits - x * bits % 8));
                }
                append(tiles_out, packed);
            }
        } else if (mode == Zrle::PaletteRle && c.size() <= 127) {
            tiles_out.push_back(std::uint8_t(128 + c.size()));
            cpixels(c);
            for (const auto& [p, n] : runs) {
                if (n == 1) {
                    tiles_out.push_back(std::uint8_t(index(p)));
                } else {
                    tiles_out.push_back(std::uint8_t(index(p) | 0x80U));
                    run(tiles_out, n);
                }
            }
        } else if (mode == Zrle::PlainRle) {
            tiles_out.push_back(128);
            for (const auto& [p, n] : runs) {
                append(tiles_out, p.first(3));
                run(tiles_out, n);
            }
        } else {
            tiles_out.push_back(0);
            cpixels(ps);
        }
    }
    const Bytes compressed = z.sync(tiles_out);
    Bytes out;
    be32(out, std::uint32_t(compressed.size()));
    append(out, compressed);
    return out;
}

std::vector<std::uint8_t> rect(std::uint16_t x, std::uint16_t y, std::uint16_t w, std::uint16_t h, std::int32_t encoding,
                               std::span<const std::uint8_t> payload) {
    Bytes out;
    for (const std::uint16_t f : {x, y, w, h}) be16(out, f);
    be32(out, std::uint32_t(encoding));
    append(out, payload);
    return out;
}

std::vector<std::uint8_t> update(const std::vector<std::vector<std::uint8_t>>& rects) {
    Bytes out{0, 0};
    be16(out, std::uint16_t(rects.size()));
    for (const auto& r : rects) append(out, r);
    return out;
}

asio::awaitable<FakeRfbServer> fake_rfb(std::uint16_t width, std::uint16_t height, std::array<std::uint8_t, 3> color) {
    using Signal = asio::experimental::channel<void(asio::error_code)>;
    const auto executor = co_await asio::this_coro::executor;
    auto listener = std::make_shared<asio::ip::tcp::acceptor>(executor, asio::ip::tcp::endpoint(asio::ip::make_address("127.0.0.1"), 0));
    const auto address = "127.0.0.1:" + std::to_string(listener->local_endpoint().port());
    auto received = std::make_shared<Received>();
    auto update_sent = std::make_shared<Signal>(executor, 1);
    auto serve = [](std::shared_ptr<asio::ip::tcp::acceptor> listener, std::shared_ptr<Received> sink, std::shared_ptr<Signal> update_sent,
                    std::uint16_t width, std::uint16_t height, std::array<std::uint8_t, 3> color) -> asio::awaitable<void> {
        auto fake = co_await FakeRfb::accept(*listener);
        co_await fake.handshake(std::nullopt, width, height);
        co_await update_sent->async_receive(asio::as_tuple(asio::use_awaitable));
        const auto picture = RgbaImage::filled(width, height, Pixel{color[0], color[1], color[2], 255});
        const auto frame = update({rect(0, 0, width, height, 0, raw(picture))});
        co_await fake.send(frame);
        std::array<std::uint8_t, 256> chunk{};
        for (;;) {
            auto [ec, n] = co_await fake.socket().async_read_some(asio::buffer(chunk), asio::as_tuple(asio::use_awaitable));
            if (ec || n == 0) co_return;
            std::lock_guard lock(sink->mutex);
            sink->bytes.insert(sink->bytes.end(), chunk.begin(), chunk.begin() + static_cast<std::ptrdiff_t>(n));
        }
    };
    asio::co_spawn(executor, serve(listener, received, update_sent, width, height, color), asio::detached);
    auto send_update = [update_sent] { asio::post(update_sent->get_executor(), [update_sent] { update_sent->try_send(asio::error_code{}); }); };
    co_return FakeRfbServer{address, std::move(received), std::move(send_update)};
}

} // namespace loa::surfaces::testkit::rfb
