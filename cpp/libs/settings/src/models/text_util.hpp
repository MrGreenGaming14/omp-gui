#pragma once

// Rust `str`/`char` helpers the model settings port needs: Unicode `trim`,
// `char::is_whitespace` and `char::is_control`. Internal to `loa_settings`.

#include <cstdint>
#include <string_view>

namespace loa::settings::models::detail {

// Decodes the code point at `s[i]` and advances `i`; invalid bytes decode as U+FFFD.
inline std::uint32_t next_char(std::string_view s, std::size_t& i) noexcept {
    const auto c = static_cast<unsigned char>(s[i]);
    std::size_t n = c < 0x80 ? 0 : (c & 0xE0) == 0xC0 ? 1 : (c & 0xF0) == 0xE0 ? 2 : (c & 0xF8) == 0xF0 ? 3 : 0;
    std::uint32_t cp = n == 0 ? (c < 0x80 ? c : 0xFFFD) : n == 1 ? (c & 0x1F) : n == 2 ? (c & 0x0F) : (c & 0x07);
    ++i;
    for (std::size_t k = 0; k < n && i < s.size(); ++k, ++i) {
        cp = (cp << 6) | (static_cast<unsigned char>(s[i]) & 0x3F);
    }
    return cp;
}

// Rust `char::is_whitespace` (Unicode White_Space).
inline bool is_whitespace(std::uint32_t c) noexcept {
    return (c >= 0x09 && c <= 0x0D) || c == 0x20 || c == 0x85 || c == 0xA0 || c == 0x1680 || (c >= 0x2000 && c <= 0x200A) ||
           c == 0x2028 || c == 0x2029 || c == 0x202F || c == 0x205F || c == 0x3000;
}

// Rust `char::is_control` (general category Cc).
inline bool is_control(std::uint32_t c) noexcept { return c < 0x20 || (c >= 0x7F && c <= 0x9F); }

// Whether any char of `s` satisfies `pred`.
template <typename Pred>
bool any_char(std::string_view s, Pred pred) {
    for (std::size_t i = 0; i < s.size();) {
        if (pred(next_char(s, i))) {
            return true;
        }
    }
    return false;
}

inline std::string_view trim_start(std::string_view s) noexcept {
    std::size_t i = 0;
    while (i < s.size()) {
        std::size_t j = i;
        if (!is_whitespace(next_char(s, j))) {
            break;
        }
        i = j;
    }
    return s.substr(i);
}

inline std::string_view trim_end(std::string_view s) noexcept {
    std::size_t end = 0;
    for (std::size_t i = 0; i < s.size();) {
        if (!is_whitespace(next_char(s, i))) {
            end = i;
        }
    }
    return s.substr(0, end);
}

// Rust `str::trim`.
inline std::string_view trim(std::string_view s) noexcept { return trim_end(trim_start(s)); }

} // namespace loa::settings::models::detail
