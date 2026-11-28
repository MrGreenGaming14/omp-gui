#pragma once

// Private: the parts of Rust's `char` API the account helpers need, over UTF-8 text.

#include <cstddef>
#include <string_view>

namespace loa::harness::account_support::detail {

// Decodes the code point at `i` and advances `i`; invalid bytes decode as themselves.
inline char32_t next_char(std::string_view s, std::size_t& i) {
    const auto c = static_cast<unsigned char>(s[i]);
    std::size_t len = c < 0x80 ? 1 : c >= 0xF0 ? 4 : c >= 0xE0 ? 3 : c >= 0xC0 ? 2 : 1;
    if (i + len > s.size()) len = 1;
    char32_t cp = len == 1 ? c : len == 2 ? (c & 0x1F) : len == 3 ? (c & 0x0F) : (c & 0x07);
    for (std::size_t k = 1; k < len; ++k) cp = (cp << 6) | (static_cast<unsigned char>(s[i + k]) & 0x3F);
    i += len;
    return cp;
}

// `char::is_whitespace`.
inline bool is_whitespace(char32_t c) {
    return (c >= 0x09 && c <= 0x0D) || c == 0x20 || c == 0x85 || c == 0xA0 || c == 0x1680 || (c >= 0x2000 && c <= 0x200A) ||
           c == 0x2028 || c == 0x2029 || c == 0x202F || c == 0x205F || c == 0x3000;
}

// `char::is_control`: the Cc category.
inline bool is_control(char32_t c) { return c < 0x20 || (c >= 0x7F && c <= 0x9F); }

// `str::trim`.
inline std::string_view trim(std::string_view s) {
    std::size_t start = 0;
    while (start < s.size()) {
        std::size_t i = start;
        if (!is_whitespace(next_char(s, i))) break;
        start = i;
    }
    std::size_t end = start;
    for (std::size_t i = start; i < s.size();) {
        if (!is_whitespace(next_char(s, i))) end = i;
    }
    return s.substr(start, end - start);
}

} // namespace loa::harness::account_support::detail
