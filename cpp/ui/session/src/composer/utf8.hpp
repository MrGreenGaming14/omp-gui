#pragma once

// UTF-8 helpers of the composer: decoding, Rust `char::is_whitespace` and `is_alphanumeric`
// (approximated: every non-ASCII letter-like code point counts as alphanumeric), trimming and
// grapheme boundaries (a base code point and its combining marks, joiners and selectors).

#include <cstddef>
#include <string>
#include <string_view>

namespace loa::ui::session::composer::utf8 {

// Decodes the code point at `i`, advancing `i`; invalid bytes decode as U+FFFD.
[[nodiscard]] inline char32_t next(std::string_view s, std::size_t& i) {
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

// The start of the code point before byte `i` (`i > 0`).
[[nodiscard]] inline std::size_t prev_boundary(std::string_view s, std::size_t i) {
    if (i == 0) return 0;
    --i;
    while (i > 0 && (static_cast<unsigned char>(s[i]) & 0xC0) == 0x80) --i;
    return i;
}

// The code point before byte `i` (`i > 0`).
[[nodiscard]] inline char32_t prev(std::string_view s, std::size_t i) {
    auto at = prev_boundary(s, i);
    return next(s, at);
}

// Whether `i` falls on a code point boundary.
[[nodiscard]] inline bool is_boundary(std::string_view s, std::size_t i) {
    return i == 0 || i >= s.size() || (static_cast<unsigned char>(s[i]) & 0xC0) != 0x80;
}

inline void push(std::string& out, char32_t c) {
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

[[nodiscard]] inline std::size_t len(char32_t c) { return c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4; }

// Rust `char::is_whitespace` (Unicode White_Space).
[[nodiscard]] constexpr bool is_space(char32_t c) {
    return (c >= 0x09 && c <= 0x0D) || c == 0x20 || c == 0x85 || c == 0xA0 || c == 0x1680 || (c >= 0x2000 && c <= 0x200A) ||
           c == 0x2028 || c == 0x2029 || c == 0x202F || c == 0x205F || c == 0x3000;
}

// Rust `char::is_alphanumeric`, approximated for non-ASCII: anything that is not whitespace,
// general punctuation or a symbol block counts.
[[nodiscard]] constexpr bool is_alnum(char32_t c) {
    if (c < 0x80) return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
    if (is_space(c)) return false;
    if (c < 0xC0) return c == 0xAA || c == 0xB5 || c == 0xBA || c == 0xB2 || c == 0xB3 || c == 0xB9 || (c >= 0xBC && c <= 0xBE);
    if (c == 0xD7 || c == 0xF7) return false;
    if (c >= 0x2000 && c <= 0x2BFF) return false; // punctuation, symbols, arrows, shapes
    if (c >= 0x3000 && c <= 0x303F) return false;
    if (c >= 0x1F000 && c <= 0x1FAFF) return false; // emoji
    if (c >= 0x0300 && c <= 0x036F) return false;  // combining marks are not alphanumeric
    return true;
}

// Whether `c` attaches to the code point before it inside one grapheme.
[[nodiscard]] constexpr bool is_extend(char32_t c) {
    return (c >= 0x0300 && c <= 0x036F) || (c >= 0x1AB0 && c <= 0x1AFF) || (c >= 0x1DC0 && c <= 0x1DFF) ||
           (c >= 0x20D0 && c <= 0x20FF) || (c >= 0xFE00 && c <= 0xFE0F) || (c >= 0xFE20 && c <= 0xFE2F) || c == 0x200D ||
           (c >= 0x1F3FB && c <= 0x1F3FF) || (c >= 0xE0100 && c <= 0xE01EF);
}

// The end of the grapheme starting at byte `i`.
[[nodiscard]] inline std::size_t grapheme_end(std::string_view s, std::size_t i) {
    bool joined = false;
    (void)next(s, i);
    while (i < s.size()) {
        auto j = i;
        const auto c = next(s, j);
        if (joined || is_extend(c)) {
            joined = c == 0x200D;
            i = j;
            continue;
        }
        break;
    }
    return i;
}

// Number of code points of `s`.
[[nodiscard]] inline std::size_t count(std::string_view s) {
    std::size_t n = 0;
    for (std::size_t i = 0; i < s.size();) {
        (void)next(s, i);
        ++n;
    }
    return n;
}

// `s` without leading and trailing whitespace (Rust `str::trim`).
[[nodiscard]] inline std::string_view trim_end(std::string_view s) {
    while (!s.empty()) {
        const auto at = prev_boundary(s, s.size());
        auto i = at;
        if (!is_space(next(s, i))) break;
        s = s.substr(0, at);
    }
    return s;
}

[[nodiscard]] inline std::string_view trim_start(std::string_view s) {
    while (!s.empty()) {
        std::size_t i = 0;
        if (!is_space(next(s, i))) break;
        s = s.substr(i);
    }
    return s;
}

[[nodiscard]] inline std::string_view trim(std::string_view s) { return trim_end(trim_start(s)); }

// The byte offset of the first whitespace in `s` from `from`, or `npos`.
[[nodiscard]] inline std::size_t find_space(std::string_view s, std::size_t from = 0) {
    for (std::size_t i = from; i < s.size();) {
        const auto at = i;
        if (is_space(next(s, i))) return at;
    }
    return std::string_view::npos;
}

// `s` with every `from` replaced by `to`.
[[nodiscard]] inline std::string replace_all(std::string_view s, std::string_view from, std::string_view to) {
    std::string out;
    std::size_t at = 0;
    for (auto hit = s.find(from); hit != std::string_view::npos && !from.empty(); hit = s.find(from, at)) {
        out.append(s.substr(at, hit - at));
        out.append(to);
        at = hit + from.size();
    }
    out.append(s.substr(at));
    return out;
}

} // namespace loa::ui::session::composer::utf8
