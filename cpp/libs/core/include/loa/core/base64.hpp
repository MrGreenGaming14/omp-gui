#pragma once

#include <cstdint>
#include <span>
#include <string>

namespace loa::core::base64 {

// Standard alphabet with `=` padding (RFC 4648 §4), appended to `out`.
void encode(std::span<const std::uint8_t> bytes, std::string& out);
[[nodiscard]] std::string encode(std::span<const std::uint8_t> bytes);

} // namespace loa::core::base64
