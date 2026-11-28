#pragma once

// Internal: SSH wire encoding (RFC 4251 `uint32`, `uint64`, `string`) for key blobs, the agent
// protocol and SFTP packets, and the base64 the `base64` crate's STANDARD engine speaks.

#include <cstdint>
#include <optional>
#include <span>
#include <string>
#include <string_view>

namespace loa::remote::wire {

using Bytes = std::string;

class Reader {
  public:
    explicit Reader(std::string_view data) : data_(data) {}
    [[nodiscard]] std::optional<std::uint8_t> u8();
    [[nodiscard]] std::optional<std::uint32_t> u32();
    [[nodiscard]] std::optional<std::uint64_t> u64();
    [[nodiscard]] std::optional<std::string_view> string();
    [[nodiscard]] std::optional<std::string_view> raw(std::size_t n);
    [[nodiscard]] std::size_t offset() const noexcept { return off_; }
    [[nodiscard]] bool empty() const noexcept { return off_ >= data_.size(); }
    [[nodiscard]] std::string_view rest() const noexcept { return data_.substr(off_); }

  private:
    std::string_view data_;
    std::size_t off_ = 0;
};

void put_u8(Bytes& out, std::uint8_t v);
void put_u32(Bytes& out, std::uint32_t v);
void put_u64(Bytes& out, std::uint64_t v);
void put_string(Bytes& out, std::string_view s);

// Standard alphabet with canonical `=` padding.
[[nodiscard]] std::string base64_encode(std::string_view bytes);
// The `base64` 0.22 STANDARD decoder, its error `Display` text included.
[[nodiscard]] std::optional<Bytes> base64_decode(std::string_view text, std::string* error = nullptr);

} // namespace loa::remote::wire
