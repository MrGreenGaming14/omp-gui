#include "loa/runtime/text.hpp"

#include <openssl/rand.h>

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <vector>

#include "utf8.hpp"

namespace loa::runtime::text {

using detail::is_char_boundary;

std::size_t floor_boundary(std::string_view s, std::size_t i) noexcept {
    for (std::size_t k = std::min(i, s.size());; --k) {
        if (is_char_boundary(s, k)) return k;
        if (k == 0) return 0;
    }
}

std::size_t ceil_boundary(std::string_view s, std::size_t i) noexcept {
    for (std::size_t k = std::min(i, s.size()); k <= s.size(); ++k)
        if (is_char_boundary(s, k)) return k;
    return s.size();
}

std::string_view clip(std::string_view s, std::size_t max) noexcept { return s.substr(0, floor_boundary(s, max)); }

std::string clip_marked(std::string_view s, std::size_t max) {
    const auto kept = clip(s, max);
    if (kept.size() == s.size()) return std::string(s);
    return std::string(kept) + "… [" + std::to_string(s.size() - kept.size()) + " more bytes]";
}

std::size_t Paginator::size(std::size_t requested) const noexcept {
    return requested == 0 ? default_size : std::min(requested, max);
}

Page Paginator::bytes(std::string_view text, std::size_t offset, std::size_t requested) const noexcept {
    const auto limit = size(requested);
    const auto start = floor_boundary(text, offset);
    const auto sum = start + limit < start ? SIZE_MAX : start + limit;
    auto end = floor_boundary(text, sum);
    if (end < text.size()) {
        const auto nl = text.substr(start, end - start).rfind('\n');
        if (nl != std::string_view::npos && nl > limit / 2) end = start + nl + 1;
        if (end == start) end = ceil_boundary(text, start + 1);
    }
    return {{start, end}, end < text.size() ? std::optional<std::size_t>(end) : std::nullopt};
}

std::string first_line(std::string_view s, std::size_t max) {
    std::string_view line;
    for (auto l : detail::lines(s)) {
        l = detail::trim(l);
        if (!l.empty()) {
            line = l;
            break;
        }
    }
    std::size_t chars = 0;
    for (std::size_t i = 0; i < line.size(); ++i) {
        if (!is_char_boundary(line, i)) continue;
        if (chars++ == max) return std::string(line.substr(0, i)) + "…";
    }
    return std::string(line);
}

std::string percent_encode(std::string_view s) {
    std::string out;
    out.reserve(s.size());
    for (const char ch : s) {
        const auto b = static_cast<unsigned char>(ch);
        const bool keep = (b >= '0' && b <= '9') || (b >= 'a' && b <= 'z') || (b >= 'A' && b <= 'Z') || b == '-' || b == '.' ||
                          b == '_' || b == '~';
        if (keep) {
            out += ch;
        } else {
            char buf[4];
            std::snprintf(buf, sizeof buf, "%%%02X", b);
            out += buf;
        }
    }
    return out;
}

std::string random_hex(std::size_t bytes) {
    std::vector<unsigned char> buf(bytes);
    // The Rust code panics when the OS random source fails; exceptions stay out of the API, so abort.
    if (bytes > 0 && RAND_bytes(buf.data(), static_cast<int>(bytes)) != 1) std::abort();
    static constexpr char hex[] = "0123456789abcdef";
    std::string out;
    out.reserve(bytes * 2);
    for (const auto b : buf) {
        out += hex[b >> 4];
        out += hex[b & 0xF];
    }
    return out;
}

std::optional<std::string_view> mime_of(const std::filesystem::path& p) {
    if (!p.has_extension()) return std::nullopt;
    auto ext = p.extension().string().substr(1);
    std::ranges::transform(ext, ext.begin(), [](unsigned char c) { return static_cast<char>(c >= 'A' && c <= 'Z' ? c + 32 : c); });
    if (ext == "png") return "image/png";
    if (ext == "jpg" || ext == "jpeg") return "image/jpeg";
    if (ext == "gif") return "image/gif";
    if (ext == "webp") return "image/webp";
    if (ext == "svg") return "image/svg+xml";
    if (ext == "pdf") return "application/pdf";
    return std::nullopt;
}

} // namespace loa::runtime::text
