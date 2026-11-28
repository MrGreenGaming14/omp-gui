#pragma once

// The one framing codec of journal files (port of `crates/loa-bus/src/frame.rs`).
//
// - A record is `[len u32 LE][crc32 u32 LE][len bytes]`; journal records hold
//   one `proto::Patch` each.
// - A file starts with a preamble, `[magic; 8][version u32 LE]`.

#include <array>
#include <cstddef>
#include <cstdint>
#include <expected>
#include <iosfwd>
#include <span>
#include <utility>
#include <vector>

#include "loa/runtime/error.hpp"

namespace loa::bus::frame {

// Bytes before a record's payload.
inline constexpr std::uint64_t header_len = 8;
// Largest payload a record may hold. A larger length field is damage.
inline constexpr std::uint32_t max_payload = std::uint32_t{1} << 28;
// A sparse offset index entry is kept every this many records.
inline constexpr std::uint64_t checkpoint_every = 1024;
// Bytes of the magic and version that open every file.
inline constexpr std::size_t preamble_len = 12;

using Bytes = std::vector<std::uint8_t>;
using Magic = std::array<std::uint8_t, 8>;

[[nodiscard]] std::uint32_t le_u32(std::span<const std::uint8_t> b, std::size_t i) noexcept;
[[nodiscard]] std::uint64_t le_u64(std::span<const std::uint8_t> b, std::size_t i) noexcept;
void put_le_u32(std::uint8_t* out, std::uint32_t v) noexcept;

// crc32 (IEEE, as crc32fast) through zlib.
[[nodiscard]] std::uint32_t crc32(std::span<const std::uint8_t> bytes) noexcept;

[[nodiscard]] std::array<std::uint8_t, preamble_len> preamble(const Magic& magic, std::uint32_t version) noexcept;

// Fills in the length and checksum of a record whose 8 header bytes start at
// `start` and whose payload runs to the end of `buf`. On a too large payload
// returns its length and leaves `buf` as is.
std::expected<void, std::size_t> seal(Bytes& buf, std::size_t start);

// Appends one record to `buf`: `payload(buf)` appends the payload, then its
// length and checksum are filled in. Fails with the payload length when too large.
template <class F>
std::expected<void, std::size_t> put(Bytes& buf, F&& payload) {
    const std::size_t start = buf.size();
    buf.resize(start + header_len, 0);
    std::forward<F>(payload)(buf);
    return seal(buf, start);
}

// What `read` found at a record boundary.
enum class Kind : std::uint8_t {
    whole,   // a record whose payload matches its checksum
    short_,  // the header or the declared payload runs past the room left
    bad_len,
    bad_crc,
};

struct Frame {
    Kind kind = Kind::short_;
    std::uint32_t len = 0;  // declared payload length; 0 for `short_`

    friend bool operator==(const Frame&, const Frame&) = default;
};

// Reads one record that must end within `room` bytes, its payload into `buf`.
// Io failures (including a premature end of stream) are `ErrorCode::io`.
[[nodiscard]] std::expected<Frame, loa::Error> read(std::istream& in, std::uint64_t room, Bytes& buf);

// The same over bytes in memory: the room is `in.size()`. For a `whole`
// record, `payload` views its bytes inside `in`.
struct View {
    Frame frame;
    std::span<const std::uint8_t> payload;
};
[[nodiscard]] View read(std::span<const std::uint8_t> in) noexcept;

}  // namespace loa::bus::frame
