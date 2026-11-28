#include "loa/projects/text.hpp"

#include <clocale>
#include <cstdio>
#include <cwctype>
#include <locale.h>

#include <charconv>

namespace loa::projects::text {

namespace {

// glibc's C.UTF-8 carries the full Unicode case and class tables.
locale_t unicode_locale() {
    static const locale_t loc = [] {
        locale_t l = newlocale(LC_CTYPE_MASK, "C.UTF-8", static_cast<locale_t>(nullptr));
        if (!l) l = newlocale(LC_CTYPE_MASK, "en_US.UTF-8", static_cast<locale_t>(nullptr));
        return l;
    }();
    return loc;
}

bool is_alpha(char32_t c) {
    if (c < 0x80) return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
    const auto loc = unicode_locale();
    return loc && iswalpha_l(static_cast<wint_t>(c), loc);
}

char32_t lower(char32_t c) {
    if (c < 0x80) return (c >= 'A' && c <= 'Z') ? c + 32 : c;
    const auto loc = unicode_locale();
    return loc ? static_cast<char32_t>(towlower_l(static_cast<wint_t>(c), loc)) : c;
}

} // namespace

char32_t decode(std::string_view s, std::size_t& i) noexcept {
    const auto b0 = static_cast<unsigned char>(s[i]);
    auto cont = [&](std::size_t k) -> char32_t {
        return i + k < s.size() ? static_cast<unsigned char>(s[i + k]) & 0x3f : 0;
    };
    if (b0 < 0x80) {
        i += 1;
        return b0;
    }
    if ((b0 >> 5) == 0x6 && i + 1 < s.size()) {
        char32_t c = ((b0 & 0x1f) << 6) | cont(1);
        i += 2;
        return c;
    }
    if ((b0 >> 4) == 0xe && i + 2 < s.size()) {
        char32_t c = ((b0 & 0x0f) << 12) | (cont(1) << 6) | cont(2);
        i += 3;
        return c;
    }
    if ((b0 >> 3) == 0x1e && i + 3 < s.size()) {
        char32_t c = ((b0 & 0x07) << 18) | (cont(1) << 12) | (cont(2) << 6) | cont(3);
        i += 4;
        return c;
    }
    i += 1;
    return b0;
}

void encode(char32_t c, std::string& out) {
    if (c < 0x80) {
        out.push_back(static_cast<char>(c));
    } else if (c < 0x800) {
        out.push_back(static_cast<char>(0xc0 | (c >> 6)));
        out.push_back(static_cast<char>(0x80 | (c & 0x3f)));
    } else if (c < 0x10000) {
        out.push_back(static_cast<char>(0xe0 | (c >> 12)));
        out.push_back(static_cast<char>(0x80 | ((c >> 6) & 0x3f)));
        out.push_back(static_cast<char>(0x80 | (c & 0x3f)));
    } else {
        out.push_back(static_cast<char>(0xf0 | (c >> 18)));
        out.push_back(static_cast<char>(0x80 | ((c >> 12) & 0x3f)));
        out.push_back(static_cast<char>(0x80 | ((c >> 6) & 0x3f)));
        out.push_back(static_cast<char>(0x80 | (c & 0x3f)));
    }
}

bool is_whitespace(char32_t c) noexcept {
    switch (c) {
        case 0x09: case 0x0a: case 0x0b: case 0x0c: case 0x0d: case 0x20: case 0x85: case 0xa0: case 0x1680:
        case 0x2028: case 0x2029: case 0x202f: case 0x205f: case 0x3000: return true;
        default: return c >= 0x2000 && c <= 0x200a;
    }
}

bool is_alphanumeric(char32_t c) noexcept {
    if (c < 0x80) return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
    const auto loc = unicode_locale();
    return loc && iswalnum_l(static_cast<wint_t>(c), loc);
}

std::string to_lowercase(std::string_view s) {
    std::string out;
    out.reserve(s.size());
    char32_t prev = 0;
    for (std::size_t i = 0; i < s.size();) {
        const char32_t c = decode(s, i);
        if (c == 0x130) {
            // `İ` lowercases to `i` and a combining dot above.
            encode(U'i', out);
            encode(0x307, out);
        } else if (c == 0x3a3) {
            // Final sigma: after a letter and not before one.
            std::size_t j = i;
            const bool next_alpha = j < s.size() && is_alpha(decode(s, j));
            encode(prev != 0 && is_alpha(prev) && !next_alpha ? 0x3c2 : 0x3c3, out);
        } else {
            encode(lower(c), out);
        }
        prev = c;
    }
    return out;
}

std::vector<std::string_view> lines(std::string_view s) {
    std::vector<std::string_view> out;
    std::size_t start = 0;
    while (start < s.size()) {
        auto end = s.find('\n', start);
        auto line = s.substr(start, end == std::string_view::npos ? std::string_view::npos : end - start);
        if (!line.empty() && line.back() == '\r') line.remove_suffix(1);
        out.push_back(line);
        if (end == std::string_view::npos) break;
        start = end + 1;
    }
    return out;
}

std::vector<std::string_view> split_whitespace(std::string_view s) {
    std::vector<std::string_view> out;
    std::size_t word = std::string_view::npos;
    for (std::size_t i = 0; i < s.size();) {
        const std::size_t at = i;
        const char32_t c = decode(s, i);
        if (is_whitespace(c)) {
            if (word != std::string_view::npos) out.push_back(s.substr(word, at - word));
            word = std::string_view::npos;
        } else if (word == std::string_view::npos) {
            word = at;
        }
    }
    if (word != std::string_view::npos) out.push_back(s.substr(word));
    return out;
}

std::string_view trim(std::string_view s) {
    std::size_t start = 0;
    while (start < s.size()) {
        std::size_t i = start;
        if (!is_whitespace(decode(s, i))) break;
        start = i;
    }
    std::size_t end = s.size();
    while (end > start) {
        std::size_t back = end - 1;
        while (back > start && (static_cast<unsigned char>(s[back]) & 0xc0) == 0x80) --back;
        std::size_t i = back;
        if (!is_whitespace(decode(s, i))) break;
        end = back;
    }
    return s.substr(start, end - start);
}

std::string one_line(std::string_view s) {
    std::string out;
    for (auto word : split_whitespace(s)) {
        if (!out.empty()) out.push_back(' ');
        out += word;
    }
    return out;
}

std::string debug_str(std::string_view s) {
    std::string out = "\"";
    for (std::size_t i = 0; i < s.size();) {
        const char32_t c = decode(s, i);
        switch (c) {
            case U'\0': out += "\\0"; break;
            case U'\t': out += "\\t"; break;
            case U'\r': out += "\\r"; break;
            case U'\n': out += "\\n"; break;
            case U'\\': out += "\\\\"; break;
            case U'"': out += "\\\""; break;
            default:
                if (c < 0x20 || (c >= 0x7f && c < 0xa0)) {
                    char buf[16];
                    std::snprintf(buf, sizeof buf, "\\u{%x}", static_cast<unsigned>(c));
                    out += buf;
                } else {
                    encode(c, out);
                }
        }
    }
    out.push_back('"');
    return out;
}

std::optional<std::size_t> parse_usize(std::string_view s) {
    if (s.starts_with('+')) s.remove_prefix(1);
    if (s.empty()) return std::nullopt;
    std::size_t n = 0;
    auto [end, ec] = std::from_chars(s.data(), s.data() + s.size(), n);
    if (ec != std::errc{} || end != s.data() + s.size()) return std::nullopt;
    return n;
}

} // namespace loa::projects::text
