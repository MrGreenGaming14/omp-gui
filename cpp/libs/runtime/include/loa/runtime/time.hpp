#pragma once

// Wall-clock stamps, relative ages and RFC 3339 timestamps in Unix milliseconds.

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>

namespace loa::runtime::time {

// Now in Unix milliseconds.
[[nodiscard]] std::int64_t now_ms() noexcept;
// Now in Unix seconds.
[[nodiscard]] std::uint64_t now_secs() noexcept;
// Now in Unix microseconds.
[[nodiscard]] std::uint64_t now_us() noexcept;

// "just now", "5m ago", "3h ago", "2d ago" for `then_ms` seen at `now_ms`.
[[nodiscard]] std::string ago(std::int64_t now_ms, std::int64_t then_ms);
// "35m", "4h 35m", "2d 12h" for a span of `ms` milliseconds.
[[nodiscard]] std::string until(std::int64_t ms);

// An RFC 3339 timestamp (offsets allowed) as Unix milliseconds.
[[nodiscard]] std::optional<std::int64_t> parse_iso_ms(std::string_view s) noexcept;
// Unix milliseconds as `YYYY-MM-DDTHH:MM:SS.mmmZ`; empty outside years -9999..=9999.
[[nodiscard]] std::string format_iso_ms(std::int64_t ms);

} // namespace loa::runtime::time
