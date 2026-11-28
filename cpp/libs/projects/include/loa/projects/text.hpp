#pragma once

#include <optional>

// Unicode-aware string helpers with Rust `str` semantics, used by search, ranking and rendering.

#include <string>
#include <string_view>
#include <vector>

namespace loa::projects::text {

// `char::is_whitespace`.
[[nodiscard]] bool is_whitespace(char32_t c) noexcept;
// `char::is_alphanumeric`.
[[nodiscard]] bool is_alphanumeric(char32_t c) noexcept;
// `str::to_lowercase`.
[[nodiscard]] std::string to_lowercase(std::string_view s);
// `str::lines`: split on `\n`, a trailing `\r` dropped, no empty last line.
[[nodiscard]] std::vector<std::string_view> lines(std::string_view s);
// `str::split_whitespace`.
[[nodiscard]] std::vector<std::string_view> split_whitespace(std::string_view s);
// `str::trim`.
[[nodiscard]] std::string_view trim(std::string_view s);
// The words of `s` joined by single spaces.
[[nodiscard]] std::string one_line(std::string_view s);
// `format!("{s:?}")`.
[[nodiscard]] std::string debug_str(std::string_view s);
// Rust's `usize::from_str`: an optional `+`, then at least one ASCII digit, no overflow.
[[nodiscard]] std::optional<std::size_t> parse_usize(std::string_view s);

// Decodes the char at `i` (valid UTF-8 assumed; a bad byte decodes as itself) and advances `i`.
[[nodiscard]] char32_t decode(std::string_view s, std::size_t& i) noexcept;
void encode(char32_t c, std::string& out);

} // namespace loa::projects::text
