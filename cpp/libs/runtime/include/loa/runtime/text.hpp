#pragma once

// UTF-8-safe cutting and paging, URL escaping, random tokens and MIME lookup.

#include <cstddef>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>

namespace loa::runtime::text {

// The largest char boundary of `s` at or below `i`.
[[nodiscard]] std::size_t floor_boundary(std::string_view s, std::size_t i) noexcept;
// The smallest char boundary of `s` at or above `i`.
[[nodiscard]] std::size_t ceil_boundary(std::string_view s, std::size_t i) noexcept;

// The longest prefix of `s` that is at most `max` bytes and ends on a char boundary.
[[nodiscard]] std::string_view clip(std::string_view s, std::size_t max) noexcept;
// `s` clipped to `max` bytes with a marker saying how much was cut: `… [N more bytes]`.
[[nodiscard]] std::string clip_marked(std::string_view s, std::size_t max);

// A byte range `[start, end)` of a text.
struct ByteRange {
    std::size_t start = 0;
    std::size_t end = 0;
    [[nodiscard]] std::size_t size() const noexcept { return end - start; }
    [[nodiscard]] bool empty() const noexcept { return start == end; }
    friend bool operator==(const ByteRange&, const ByteRange&) = default;
};

// A page of text and the next page's offset (none on the last page).
struct Page {
    ByteRange range;
    std::optional<std::size_t> next;
};

// Page sizes: a request of 0 gets `default_size`, any other is capped at `max`.
struct Paginator {
    std::size_t default_size = 0;
    std::size_t max = 0;

    [[nodiscard]] std::size_t size(std::size_t requested) const noexcept;
    // The byte page of `text` at `offset`. Pages end on char boundaries, on a line
    // break when one is in the page's second half, and always advance.
    [[nodiscard]] Page bytes(std::string_view text, std::size_t offset, std::size_t requested) const noexcept;
    friend bool operator==(const Paginator&, const Paginator&) = default;
};

// The first non-blank line of `s`, trimmed and cut to `max` chars with a trailing `…`.
[[nodiscard]] std::string first_line(std::string_view s, std::size_t max);

// `s` with every byte outside RFC 3986 unreserved characters written as `%XX`.
[[nodiscard]] std::string percent_encode(std::string_view s);

// `bytes` bytes from the OS random source (OpenSSL RAND_bytes) as lowercase hex.
[[nodiscard]] std::string random_hex(std::size_t bytes);

// The MIME type of png, jpg, jpeg, gif, webp, svg and pdf files (extension case-insensitive).
[[nodiscard]] std::optional<std::string_view> mime_of(const std::filesystem::path& p);

} // namespace loa::runtime::text
