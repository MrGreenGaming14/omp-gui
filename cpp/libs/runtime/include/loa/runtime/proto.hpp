#pragma once

#include <cstddef>
#include <cstdint>
#include <expected>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "loa/runtime/error.hpp"

// Protobuf wire-format primitives. Message encode/decode lives in each library.
namespace loa::runtime::proto {

using Bytes = std::vector<std::uint8_t>;

enum class WireType : std::uint8_t { varint = 0, fixed64 = 1, len = 2, start_group = 3, end_group = 4, fixed32 = 5 };

[[nodiscard]] constexpr std::uint64_t zigzag_encode(std::int64_t v) noexcept {
    return (static_cast<std::uint64_t>(v) << 1) ^ static_cast<std::uint64_t>(v >> 63);
}
[[nodiscard]] constexpr std::int64_t zigzag_decode(std::uint64_t v) noexcept {
    return static_cast<std::int64_t>(v >> 1) ^ -static_cast<std::int64_t>(v & 1);
}
[[nodiscard]] constexpr std::uint32_t zigzag_encode32(std::int32_t v) noexcept {
    return (static_cast<std::uint32_t>(v) << 1) ^ static_cast<std::uint32_t>(v >> 31);
}
[[nodiscard]] constexpr std::int32_t zigzag_decode32(std::uint32_t v) noexcept {
    return static_cast<std::int32_t>(v >> 1) ^ -static_cast<std::int32_t>(v & 1);
}

void put_varint(Bytes& out, std::uint64_t v);
void put_fixed32(Bytes& out, std::uint32_t v);
void put_fixed64(Bytes& out, std::uint64_t v);
void put_tag(Bytes& out, std::uint32_t field, WireType type);
// Length prefix followed by the bytes.
void put_len(Bytes& out, std::span<const std::uint8_t> bytes);
void put_len(Bytes& out, std::string_view bytes);
[[nodiscard]] std::size_t varint_size(std::uint64_t v) noexcept;

struct Tag {
    std::uint32_t field;
    WireType type;
};

// Sequential reader over a borrowed buffer. Every read checks bounds.
class Reader {
  public:
    explicit Reader(std::span<const std::uint8_t> data) noexcept : data_(data) {}

    [[nodiscard]] bool done() const noexcept { return pos_ >= data_.size(); }
    [[nodiscard]] std::size_t position() const noexcept { return pos_; }

    [[nodiscard]] std::expected<std::uint64_t, loa::Error> varint();
    [[nodiscard]] std::expected<std::uint32_t, loa::Error> fixed32();
    [[nodiscard]] std::expected<std::uint64_t, loa::Error> fixed64();
    [[nodiscard]] std::expected<Tag, loa::Error> tag();
    // Length-delimited payload; the span borrows the reader's buffer.
    [[nodiscard]] std::expected<std::span<const std::uint8_t>, loa::Error> len();
    // Skips the value of an unknown field whose tag has already been read.
    [[nodiscard]] std::expected<void, loa::Error> skip(Tag tag);

  private:
    std::span<const std::uint8_t> data_;
    std::size_t pos_ = 0;
};

} // namespace loa::runtime::proto
