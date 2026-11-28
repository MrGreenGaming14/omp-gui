#include "loa/core/base64.hpp"

namespace loa::core::base64 {

void encode(std::span<const std::uint8_t> bytes, std::string& out) {
    static constexpr char alphabet[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    out.reserve(out.size() + (bytes.size() + 2) / 3 * 4);
    std::size_t i = 0;
    for (; i + 3 <= bytes.size(); i += 3) {
        const std::uint32_t n = (std::uint32_t{bytes[i]} << 16) | (std::uint32_t{bytes[i + 1]} << 8) | bytes[i + 2];
        out += alphabet[(n >> 18) & 63];
        out += alphabet[(n >> 12) & 63];
        out += alphabet[(n >> 6) & 63];
        out += alphabet[n & 63];
    }
    const std::size_t rest = bytes.size() - i;
    if (rest == 0) return;
    std::uint32_t n = std::uint32_t{bytes[i]} << 16;
    if (rest == 2) n |= std::uint32_t{bytes[i + 1]} << 8;
    out += alphabet[(n >> 18) & 63];
    out += alphabet[(n >> 12) & 63];
    out += rest == 2 ? alphabet[(n >> 6) & 63] : '=';
    out += '=';
}

std::string encode(std::span<const std::uint8_t> bytes) {
    std::string out;
    encode(bytes, out);
    return out;
}

} // namespace loa::core::base64
