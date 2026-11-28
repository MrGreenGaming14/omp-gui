#pragma once

// RFB rectangle decoders, one per encoding, kept in the `Decoders` registry.
// Port of `loa-surfaces/src/vnc/decode.rs`.
//
// A pixel decoder reads one rectangle into a scratch picture the size of the
// rectangle; the caller then blits it into the framebuffer.

#include <cstddef>
#include <cstdint>
#include <functional>
#include <memory>
#include <span>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/surfaces/pixels.hpp"
#include "loa/surfaces/vnc/vnc.hpp"

namespace loa::surfaces::vnc {

// Upper bound for one ZRLE rectangle's compressed data.
inline constexpr std::uint32_t MAX_ZRLE = 64U << 20U;

// A buffered byte source the decoders read from: a memory slice, or a stream
// pulled through `Source`.
class Reader {
  public:
    // Reads `buf.size()` bytes at most into `buf`; 0 at the end of the stream.
    using Source = std::function<loa::contracts::Awaitable<RfbResult<std::size_t>>(std::span<std::uint8_t> buf)>;

    // Reads only `data`, which outlives the reader.
    explicit Reader(std::span<const std::uint8_t> data);
    // Pulls from `source` through a buffer of `capacity` bytes.
    Reader(Source source, std::size_t capacity);

    // Buffered bytes not read yet; for a slice reader, what is left of it.
    [[nodiscard]] std::size_t remaining() const noexcept { return end_ - pos_; }

    // Library internal: these throw the decoder failure type on error and are
    // wrapped by the `RfbResult` entry points.
    loa::contracts::Awaitable<void> read_exact(std::span<std::uint8_t> out);
    loa::contracts::Awaitable<std::uint8_t> u8();
    loa::contracts::Awaitable<std::uint16_t> u16();
    loa::contracts::Awaitable<std::uint32_t> u32();
    loa::contracts::Awaitable<std::int32_t> i32();
    // Reads and discards `len` bytes.
    loa::contracts::Awaitable<void> skip(std::uint64_t len);

  private:
    // Refills the empty buffer; throws at the end of the stream.
    loa::contracts::Awaitable<void> refill();

    Source source_;
    std::vector<std::uint8_t> storage_;
    const std::uint8_t* data_ = nullptr;
    std::size_t pos_ = 0;
    std::size_t end_ = 0;
};

// What a decoded rectangle does to the framebuffer.
struct Update {
    enum class Kind : std::uint8_t {
        // The scratch picture holds the rectangle's pixels.
        Blit,
        // Copy the rectangle's area from `(x, y)`.
        Copy,
        // The desktop is now the rectangle's size.
        Resize,
        // A new remote pointer shape; it does not alter the framebuffer.
        Cursor,
    };

    Kind kind = Kind::Blit;
    std::uint32_t x = 0;
    std::uint32_t y = 0;
    vnc::Cursor cursor;

    friend bool operator==(const Update&, const Update&) = default;
};

// Pseudo encodings (negative numbers) describe the desktop instead of painting inside it.
[[nodiscard]] constexpr bool is_pseudo(std::int32_t encoding) { return encoding < 0; }

// The encodings the client asks for, in its order of preference: CopyRect,
// ZRLE, Hextile, RRE, Raw, Cursor, DesktopSize.
[[nodiscard]] std::vector<std::int32_t> encodings();

// The decoders by encoding number and the scratch picture they share. ZRLE
// keeps one zlib stream per connection.
class Decoders {
  public:
    Decoders();
    ~Decoders();
    Decoders(const Decoders&) = delete;
    Decoders& operator=(const Decoders&) = delete;

    [[nodiscard]] bool supports(std::int32_t encoding) const;
    // Decodes one rectangle `r` of `encoding` from `rd`.
    [[nodiscard]] loa::contracts::Awaitable<RfbResult<Update>> decode(std::int32_t encoding, Rect r, Reader& rd);

    RgbaImage scratch;

  private:
    struct Zlib;
    std::unique_ptr<Zlib> zlib_;
    std::vector<std::uint8_t> zbuf_;
    std::vector<std::uint8_t> zout_;
};

// Reads a rectangle as four u16 fields (library internal: throws).
loa::contracts::Awaitable<Rect> read_rect(Reader& rd);

} // namespace loa::surfaces::vnc
