#pragma once

// Small UTF-8 and case helpers for chords and search text (Latin, Latin-1, Greek and Cyrillic
// simple case mappings; other scripts pass through).

#include <optional>
#include <string>
#include <string_view>

namespace loa::ui::shell::detail {

[[nodiscard]] inline char32_t to_lower(char32_t c) {
    if ((c >= U'A' && c <= U'Z') || (c >= 0xC0 && c <= 0xDE && c != 0xD7) || (c >= 0x391 && c <= 0x3AB && c != 0x3A2) ||
        (c >= 0x410 && c <= 0x42F))
        return c + 0x20;
    if (c >= 0x400 && c <= 0x40F) return c + 0x50;
    return c;
}

[[nodiscard]] inline char32_t to_upper(char32_t c) {
    if ((c >= U'a' && c <= U'z') || (c >= 0xE0 && c <= 0xFE && c != 0xF7) || (c >= 0x3B1 && c <= 0x3CB && c != 0x3C2) ||
        (c >= 0x430 && c <= 0x44F))
        return c - 0x20;
    if (c >= 0x450 && c <= 0x45F) return c - 0x50;
    return c;
}

// Decodes the code point at `i`, advancing `i`; invalid bytes decode as U+FFFD.
[[nodiscard]] inline char32_t next_char(std::string_view s, std::size_t& i) {
    const auto b = static_cast<unsigned char>(s[i++]);
    if (b < 0x80) return b;
    int extra = b >= 0xF0 ? 3 : b >= 0xE0 ? 2 : b >= 0xC0 ? 1 : -1;
    if (extra < 0) return 0xFFFD;
    char32_t c = b & (0x3F >> extra);
    for (; extra > 0; --extra) {
        if (i >= s.size() || (static_cast<unsigned char>(s[i]) & 0xC0) != 0x80) return 0xFFFD;
        c = (c << 6) | (static_cast<unsigned char>(s[i++]) & 0x3F);
    }
    return c;
}

[[nodiscard]] inline std::optional<char32_t> first_char(std::string_view s) {
    if (s.empty()) return std::nullopt;
    std::size_t i = 0;
    return next_char(s, i);
}

inline void push_utf8(std::string& out, char32_t c) {
    if (c < 0x80) {
        out += static_cast<char>(c);
    } else if (c < 0x800) {
        out += static_cast<char>(0xC0 | (c >> 6));
        out += static_cast<char>(0x80 | (c & 0x3F));
    } else if (c < 0x10000) {
        out += static_cast<char>(0xE0 | (c >> 12));
        out += static_cast<char>(0x80 | ((c >> 6) & 0x3F));
        out += static_cast<char>(0x80 | (c & 0x3F));
    } else {
        out += static_cast<char>(0xF0 | (c >> 18));
        out += static_cast<char>(0x80 | ((c >> 12) & 0x3F));
        out += static_cast<char>(0x80 | ((c >> 6) & 0x3F));
        out += static_cast<char>(0x80 | (c & 0x3F));
    }
}

[[nodiscard]] inline std::string lowercase(std::string_view s) {
    std::string out;
    out.reserve(s.size());
    for (std::size_t i = 0; i < s.size();) push_utf8(out, to_lower(next_char(s, i)));
    return out;
}

} // namespace loa::ui::shell::detail
