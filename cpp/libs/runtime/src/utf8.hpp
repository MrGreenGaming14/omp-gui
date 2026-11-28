#pragma once

// Private UTF-8 helpers shared by text.cpp, diff.cpp and ssh.cpp.

#include <cstddef>
#include <string_view>
#include <vector>

namespace loa::runtime::detail {

[[nodiscard]] inline bool is_char_boundary(std::string_view s, std::size_t i) noexcept {
    if (i == 0 || i >= s.size()) return i <= s.size();
    return (static_cast<unsigned char>(s[i]) & 0xC0) != 0x80;
}

// Decodes the char starting at `i`; `width` gets its byte length (1 for invalid bytes).
[[nodiscard]] inline char32_t decode(std::string_view s, std::size_t i, std::size_t& width) noexcept {
    const auto b = static_cast<unsigned char>(s[i]);
    std::size_t n = b < 0x80 ? 1 : (b >> 5) == 0x6 ? 2 : (b >> 4) == 0xE ? 3 : (b >> 3) == 0x1E ? 4 : 1;
    if (i + n > s.size()) n = 1;
    width = n;
    if (n == 1) return b;
    char32_t c = b & (0xFF >> (n + 1));
    for (std::size_t k = 1; k < n; ++k) c = (c << 6) | (static_cast<unsigned char>(s[i + k]) & 0x3F);
    return c;
}

// Rust's `char::is_whitespace` (Unicode White_Space).
[[nodiscard]] inline bool is_whitespace(char32_t c) noexcept {
    return (c >= 0x09 && c <= 0x0D) || c == 0x20 || c == 0x85 || c == 0xA0 || c == 0x1680 || (c >= 0x2000 && c <= 0x200A) ||
           c == 0x2028 || c == 0x2029 || c == 0x202F || c == 0x205F || c == 0x3000;
}

// Rust's `str::trim`.
[[nodiscard]] inline std::string_view trim(std::string_view s) noexcept {
    std::size_t b = 0;
    while (b < s.size()) {
        std::size_t w = 0;
        if (!is_whitespace(decode(s, b, w))) break;
        b += w;
    }
    std::size_t e = s.size();
    while (e > b) {
        std::size_t start = e - 1;
        while (start > b && !is_char_boundary(s, start)) --start;
        std::size_t w = 0;
        if (!is_whitespace(decode(s, start, w))) break;
        e = start;
    }
    return s.substr(b, e - b);
}

// Rust's `str::lines`: split on `\n`, a trailing `\r` dropped, no final empty line.
[[nodiscard]] inline std::vector<std::string_view> lines(std::string_view s) {
    std::vector<std::string_view> out;
    std::size_t pos = 0;
    while (pos < s.size()) {
        auto nl = s.find('\n', pos);
        auto end = nl == std::string_view::npos ? s.size() : nl;
        auto line = s.substr(pos, end - pos);
        if (nl != std::string_view::npos && line.ends_with('\r')) line.remove_suffix(1);
        out.push_back(line);
        pos = nl == std::string_view::npos ? s.size() : nl + 1;
    }
    return out;
}

} // namespace loa::runtime::detail
