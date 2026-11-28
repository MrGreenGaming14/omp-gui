#pragma once

// Internal: UTF-8 checks the way Rust's `String::from_utf8` and `from_utf8_lossy` do them.

#include <cstddef>
#include <cstdint>
#include <string>
#include <string_view>

namespace loa::remote::text {

// Length of the valid sequence at `i`, or 0 when it is invalid; `bad` gets the length of the
// maximal invalid prefix (at least 1) to replace.
inline std::size_t sequence(std::string_view s, std::size_t i, std::size_t& bad) {
    const auto b = [&](std::size_t k) { return static_cast<std::uint8_t>(s[k]); };
    const auto c = b(i);
    bad = 1;
    if (c < 0x80) return 1;
    std::size_t len = 0;
    std::uint8_t lo = 0x80, hi = 0xbf;
    if (c >= 0xc2 && c <= 0xdf) len = 2;
    else if (c >= 0xe0 && c <= 0xef) {
        len = 3;
        if (c == 0xe0) lo = 0xa0;
        if (c == 0xed) hi = 0x9f;
    } else if (c >= 0xf0 && c <= 0xf4) {
        len = 4;
        if (c == 0xf0) lo = 0x90;
        if (c == 0xf4) hi = 0x8f;
    } else {
        return 0;
    }
    for (std::size_t k = 1; k < len; ++k) {
        if (i + k >= s.size()) return 0;
        const auto d = b(i + k);
        if (d < (k == 1 ? lo : 0x80) || d > (k == 1 ? hi : 0xbf)) return 0;
        bad = k + 1;
    }
    return len;
}

[[nodiscard]] inline bool valid_utf8(std::string_view s) {
    for (std::size_t i = 0; i < s.size();) {
        std::size_t bad = 0;
        const auto n = sequence(s, i, bad);
        if (n == 0) return false;
        i += n;
    }
    return true;
}

// Invalid sequences become U+FFFD.
[[nodiscard]] inline std::string utf8_lossy(std::string_view s) {
    std::string out;
    out.reserve(s.size());
    for (std::size_t i = 0; i < s.size();) {
        std::size_t bad = 0;
        const auto n = sequence(s, i, bad);
        if (n == 0) {
            out += "\xEF\xBF\xBD";
            i += bad;
        } else {
            out.append(s.substr(i, n));
            i += n;
        }
    }
    return out;
}

// The first `n` characters of `s` (all of it when shorter).
[[nodiscard]] inline std::string_view prefix_chars(std::string_view s, std::size_t n) {
    std::size_t i = 0;
    for (std::size_t count = 0; i < s.size() && count < n; ++count) {
        ++i;
        while (i < s.size() && (static_cast<std::uint8_t>(s[i]) & 0xc0) == 0x80) ++i;
    }
    return s.substr(0, i);
}

} // namespace loa::remote::text
