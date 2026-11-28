#pragma once

// Private: Rust's `String::from_utf8_lossy` (one U+FFFD per maximal invalid subpart).

#include <cstddef>
#include <string>
#include <string_view>

namespace loa::harness::kit::detail {

inline std::string utf8_lossy(std::string_view s) {
    std::string out;
    out.reserve(s.size());
    const auto* b = reinterpret_cast<const unsigned char*>(s.data());
    const std::size_t n = s.size();
    std::size_t i = 0;
    while (i < n) {
        const unsigned char c = b[i];
        if (c < 0x80) {
            out.push_back(static_cast<char>(c));
            ++i;
            continue;
        }
        std::size_t need = 0;
        unsigned char lo = 0x80, hi = 0xBF;
        if (c >= 0xC2 && c <= 0xDF) need = 1;
        else if (c == 0xE0) { need = 2; lo = 0xA0; }
        else if (c >= 0xE1 && c <= 0xEC) need = 2;
        else if (c == 0xED) { need = 2; hi = 0x9F; }
        else if (c >= 0xEE && c <= 0xEF) need = 2;
        else if (c == 0xF0) { need = 3; lo = 0x90; }
        else if (c >= 0xF1 && c <= 0xF3) need = 3;
        else if (c == 0xF4) { need = 3; hi = 0x8F; }
        std::size_t len = 1;
        bool ok = need > 0;
        for (std::size_t k = 0; ok && k < need; ++k) {
            if (i + len >= n) { ok = false; break; }
            const unsigned char cc = b[i + len];
            const unsigned char l = k == 0 ? lo : 0x80, h = k == 0 ? hi : 0xBF;
            if (cc < l || cc > h) { ok = false; break; }
            ++len;
        }
        if (ok) out.append(s.substr(i, len));
        else out.append("\xEF\xBF\xBD");
        i += len;
    }
    return out;
}

} // namespace loa::harness::kit::detail
