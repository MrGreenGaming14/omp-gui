// RFB rectangle decoders. Port of `loa-surfaces/src/vnc/decode.rs`.

#include "loa/surfaces/vnc/decode.hpp"

#include <zlib.h>

#include <asio/awaitable.hpp>

#include <algorithm>
#include <array>
#include <cstring>
#include <format>

#include "internal.hpp"
#include "loa/surfaces/vnc/proto.hpp"

namespace loa::surfaces::vnc {

using detail::fail;

namespace {

RfbError early_eof() { return RfbError::io("early eof"); }

void reset(RgbaImage& out, Rect r) {
    out.width = r.width;
    out.height = r.height;
    out.data.assign(std::size_t(r.width) * r.height * BPP, 0);
}

// Wire pixels (`R G B X`) to opaque RGBA in place.
void make_opaque(std::span<std::uint8_t> buf) {
    for (std::size_t i = 3; i < buf.size(); i += BPP) buf[i] = 255;
}

// Reads wire pixels into `buf` as opaque RGBA.
asio::awaitable<void> read_opaque(Reader& rd, std::span<std::uint8_t> buf) {
    co_await rd.read_exact(buf);
    make_opaque(buf);
}

asio::awaitable<Pixel> read_pixel(Reader& rd) {
    Pixel px{};
    co_await read_opaque(rd, px);
    co_return px;
}

void check_inside(Rect s, std::uint32_t width, std::uint32_t height, std::string_view what) {
    if (!region_fits(s, width, height)) {
        fail(RfbError::protocol(std::format("{} subrectangle {} outside {}x{}", what, detail::rect_debug(s), width, height)));
    }
}

// Bounds checked reader over inflated ZRLE data.
struct TileData {
    std::span<const std::uint8_t> rest;

    std::span<const std::uint8_t> take(std::size_t n) {
        if (rest.size() < n) fail(RfbError::protocol("ZRLE data ends inside a tile"));
        auto head = rest.first(n);
        rest = rest.subspan(n);
        return head;
    }
    std::uint8_t u8() { return take(1)[0]; }
    Pixel cpixel() {
        const auto p = take(3);
        return {p[0], p[1], p[2], 255};
    }
    // A run length: bytes summed until one is not 255, plus one.
    std::size_t run() {
        std::size_t len = 1;
        for (;;) {
            const std::uint8_t b = u8();
            len += b;
            if (b != 255) return len;
        }
    }
    void palette(std::array<Pixel, 127>& palette, std::size_t size) {
        for (std::size_t i = 0; i < size; ++i) palette[i] = cpixel();
    }
};

RfbError bad_index(std::size_t index) { return RfbError::protocol(std::format("ZRLE palette index {}", index)); }

// Decodes the inflated tiles of one ZRLE rectangle into `out`.
void decode_zrle_tiles(std::span<const std::uint8_t> data, RgbaImage& out) {
    const std::uint32_t width = out.width;
    const std::uint32_t height = out.height;
    TileData rd{data};
    std::array<Pixel, 127> palette{};
    for (std::uint32_t ty = 0; ty < height; ty += 64) {
        for (std::uint32_t tx = 0; tx < width; tx += 64) {
            const Rect tile{tx, ty, std::min(width - tx, 64U), std::min(height - ty, 64U)};
            const std::size_t tw = tile.width;
            const std::size_t count = tw * tile.height;
            const auto put = [&](std::size_t i, const Pixel& px) {
                const std::size_t at = ((tile.y + i / tw) * std::size_t(width) + tile.x + i % tw) * BPP;
                std::memcpy(out.data.data() + at, px.data(), BPP);
            };
            const std::uint8_t sub = rd.u8();
            if (sub == 0) {
                for (std::size_t i = 0; i < count; ++i) put(i, rd.cpixel());
            } else if (sub == 1) {
                const Pixel px = rd.cpixel();
                for (std::size_t i = 0; i < count; ++i) put(i, px);
            } else if (sub <= 16) {
                const std::size_t size = sub;
                rd.palette(palette, size);
                const std::size_t bits = size == 2 ? 1 : size <= 4 ? 2 : 4;
                for (std::size_t y = 0; y < tile.height; ++y) {
                    const auto row = rd.take((tw * bits + 7) / 8);
                    for (std::size_t x = 0; x < tw; ++x) {
                        const std::size_t bit = x * bits;
                        const std::size_t index = (row[bit / 8] >> (8 - bits - bit % 8)) & ((1U << bits) - 1);
                        if (index >= size) fail(bad_index(index));
                        put(y * tw + x, palette[index]);
                    }
                }
            } else if (sub == 128 || sub >= 130) {
                const std::size_t size = sub - 128U;
                rd.palette(palette, size);
                std::size_t i = 0;
                while (i < count) {
                    Pixel px{};
                    std::size_t run = 1;
                    if (size == 0) {
                        px = rd.cpixel();
                        run = rd.run();
                    } else {
                        const std::uint8_t b = rd.u8();
                        const std::size_t index = b & 0x7FU;
                        if (index >= size) fail(bad_index(index));
                        px = palette[index];
                        if ((b & 0x80U) != 0) run = rd.run();
                    }
                    if (i + run > count) fail(RfbError::protocol("ZRLE run past the end of a tile"));
                    for (std::size_t j = i; j < i + run; ++j) put(j, px);
                    i += run;
                }
            } else {
                fail(RfbError::protocol(std::format("ZRLE subencoding {}", sub)));
            }
        }
    }
}

} // namespace

// ---- Reader ----

Reader::Reader(std::span<const std::uint8_t> data) : data_(data.data()), end_(data.size()) {}

Reader::Reader(Source source, std::size_t capacity) : source_(std::move(source)), storage_(std::max<std::size_t>(capacity, 1)) {
    data_ = storage_.data();
}

asio::awaitable<void> Reader::refill() {
    if (!source_) fail(early_eof());
    auto n = co_await source_(storage_);
    if (!n) fail(n.error());
    if (*n == 0) fail(early_eof());
    data_ = storage_.data();
    pos_ = 0;
    end_ = *n;
}

asio::awaitable<void> Reader::read_exact(std::span<std::uint8_t> out) {
    std::size_t done = 0;
    while (done < out.size()) {
        if (pos_ == end_) co_await refill();
        const std::size_t n = std::min(end_ - pos_, out.size() - done);
        std::memcpy(out.data() + done, data_ + pos_, n);
        pos_ += n;
        done += n;
    }
}

asio::awaitable<std::uint8_t> Reader::u8() {
    if (pos_ == end_) co_await refill();
    co_return data_[pos_++];
}

asio::awaitable<std::uint16_t> Reader::u16() {
    std::array<std::uint8_t, 2> b{};
    co_await read_exact(b);
    co_return std::uint16_t(b[0] << 8U | b[1]);
}

asio::awaitable<std::uint32_t> Reader::u32() {
    std::array<std::uint8_t, 4> b{};
    co_await read_exact(b);
    co_return std::uint32_t(b[0]) << 24U | std::uint32_t(b[1]) << 16U | std::uint32_t(b[2]) << 8U | b[3];
}

asio::awaitable<std::int32_t> Reader::i32() { co_return std::int32_t(co_await u32()); }

asio::awaitable<void> Reader::skip(std::uint64_t len) {
    while (len > 0) {
        if (pos_ == end_) co_await refill();
        const std::size_t n = std::size_t(std::min<std::uint64_t>(end_ - pos_, len));
        pos_ += n;
        len -= n;
    }
}

asio::awaitable<Rect> read_rect(Reader& rd) {
    Rect r;
    r.x = co_await rd.u16();
    r.y = co_await rd.u16();
    r.width = co_await rd.u16();
    r.height = co_await rd.u16();
    co_return r;
}

// ---- Decoders ----

struct Decoders::Zlib {
    z_stream z{};
    Zlib() { inflateInit(&z); }
    ~Zlib() { inflateEnd(&z); }
    Zlib(const Zlib&) = delete;
    Zlib& operator=(const Zlib&) = delete;

    // Inflates all of `input` (one sync flushed chunk of the stream) into `out`.
    void inflate(std::span<const std::uint8_t> input, std::vector<std::uint8_t>& out) {
        std::size_t used = 0;
        out.resize(std::max<std::size_t>(out.capacity(), std::max<std::size_t>(input.size() * 4, 4096)));
        z.next_in = const_cast<Bytef*>(input.data());
        z.avail_in = uInt(input.size());
        for (;;) {
            if (used == out.size()) out.resize(out.size() * 2);
            z.next_out = out.data() + used;
            z.avail_out = uInt(out.size() - used);
            const uInt in_before = z.avail_in;
            const std::size_t out_before = used;
            const int status = ::inflate(&z, Z_SYNC_FLUSH);
            used = out.size() - z.avail_out;
            if (status != Z_OK && status != Z_STREAM_END && status != Z_BUF_ERROR) {
                fail(RfbError::zlib(z.msg != nullptr ? z.msg : "deflate decompression error"));
            }
            const bool room_left = used < out.size();
            if (status == Z_STREAM_END || (z.avail_in == 0 && room_left)) break;
            if (z.avail_in == in_before && used == out_before && room_left) {
                fail(RfbError::protocol("ZRLE zlib stream stalled"));
            }
        }
        out.resize(used);
    }
};

std::vector<std::int32_t> encodings() { return {1, 16, 5, 2, 0, -239, -223}; }

Decoders::Decoders() : zlib_(std::make_unique<Zlib>()) {}
Decoders::~Decoders() = default;

bool Decoders::supports(std::int32_t encoding) const {
    const auto list = encodings();
    return std::ranges::find(list, encoding) != list.end();
}

asio::awaitable<RfbResult<Update>> Decoders::decode(std::int32_t encoding, Rect r, Reader& rd) {
    try {
        RgbaImage& out = scratch;
        Update update;
        switch (encoding) {
        case 0: // Raw
            reset(out, r);
            co_await read_opaque(rd, out.data);
            break;
        case 1: // CopyRect
            update.kind = Update::Kind::Copy;
            update.x = co_await rd.u16();
            update.y = co_await rd.u16();
            break;
        case 2: { // RRE: a background pixel, then solid subrectangles.
            reset(out, r);
            const std::uint32_t count = co_await rd.u32();
            const Pixel bg = co_await read_pixel(rd);
            out.fill(out.full(), bg);
            for (std::uint32_t i = 0; i < count; ++i) {
                const Pixel px = co_await read_pixel(rd);
                const Rect s = co_await read_rect(rd);
                check_inside(s, r.width, r.height, "RRE");
                out.fill(s, px);
            }
            break;
        }
        case 5: { // Hextile: 16x16 tiles, left to right, top to bottom.
            constexpr std::uint8_t RAW = 1;
            constexpr std::uint8_t BACKGROUND = 2;
            constexpr std::uint8_t FOREGROUND = 4;
            constexpr std::uint8_t ANY_SUBRECTS = 8;
            constexpr std::uint8_t COLOURED = 16;
            reset(out, r);
            Pixel bg{0, 0, 0, 255};
            Pixel fg{0, 0, 0, 255};
            for (std::uint32_t ty = 0; ty < r.height; ty += 16) {
                for (std::uint32_t tx = 0; tx < r.width; tx += 16) {
                    const Rect tile{tx, ty, std::min(r.width - tx, 16U), std::min(r.height - ty, 16U)};
                    const std::uint8_t flags = co_await rd.u8();
                    if ((flags & RAW) != 0) {
                        for (std::uint32_t y = tile.y; y < tile.y + tile.height; ++y) {
                            const std::size_t start = (std::size_t(y) * r.width + tile.x) * BPP;
                            co_await read_opaque(rd, std::span(out.data).subspan(start, std::size_t(tile.width) * BPP));
                        }
                        continue;
                    }
                    if ((flags & BACKGROUND) != 0) bg = co_await read_pixel(rd);
                    if ((flags & FOREGROUND) != 0) fg = co_await read_pixel(rd);
                    out.fill(tile, bg);
                    if ((flags & ANY_SUBRECTS) == 0) continue;
                    const std::uint8_t n = co_await rd.u8();
                    for (std::uint8_t i = 0; i < n; ++i) {
                        const Pixel px = (flags & COLOURED) != 0 ? co_await read_pixel(rd) : fg;
                        const std::uint8_t xy = co_await rd.u8();
                        const std::uint8_t wh = co_await rd.u8();
                        const Rect s{std::uint32_t(xy >> 4U), std::uint32_t(xy & 0xFU), std::uint32_t(wh >> 4U) + 1,
                                     std::uint32_t(wh & 0xFU) + 1};
                        check_inside(s, tile.width, tile.height, "Hextile");
                        out.fill(Rect{tile.x + s.x, tile.y + s.y, s.width, s.height}, px);
                    }
                }
            }
            break;
        }
        case 16: { // ZRLE: zlib compressed 64x64 tiles of CPIXELs (`R G B`).
            const std::uint32_t len = co_await rd.u32();
            if (len > MAX_ZRLE) fail(RfbError::protocol(std::format("ZRLE rectangle of {} bytes", len)));
            zbuf_.assign(len, 0);
            co_await rd.read_exact(zbuf_);
            zlib_->inflate(zbuf_, zout_);
            reset(out, r);
            decode_zrle_tiles(zout_, out);
            break;
        }
        case -239: { // Cursor: pixels, then a row-padded 1 bit per pixel mask.
            reset(out, r);
            co_await read_opaque(rd, out.data);
            const std::size_t mask_row = (std::size_t(r.width) + 7) / 8;
            std::vector<std::uint8_t> mask(mask_row * r.height);
            co_await rd.read_exact(mask);
            for (std::size_t i = 0; i < std::size_t(r.width) * r.height; ++i) {
                const std::size_t x = i % r.width;
                const std::size_t y = i / r.width;
                out.data[i * BPP + 3] = (mask[y * mask_row + x / 8] & (0x80U >> (x % 8))) != 0 ? 255 : 0;
            }
            update.kind = Update::Kind::Cursor;
            update.cursor = Cursor{r.x, r.y, r.width, r.height, std::move(out.data)};
            out.data.clear();
            break;
        }
        case -223: // DesktopSize
            update.kind = Update::Kind::Resize;
            break;
        default: fail(RfbError::protocol(std::format("unrequested encoding {}", encoding)));
        }
        co_return update;
    } catch (detail::Failure& f) {
        co_return std::unexpected(std::move(f.error));
    }
}

} // namespace loa::surfaces::vnc
