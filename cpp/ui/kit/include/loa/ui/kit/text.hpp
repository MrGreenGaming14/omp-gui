#pragma once

// Text helpers for views: fuzzy filtering and highlighting, durations, sizes and truncation,
// and measured single-line ellipsis. Port of `loa-ui-kit/src/text`.

#include <cstddef>
#include <cstdint>
#include <functional>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/ui/kit/theme.hpp"

namespace loa::ui::kit::text {

// ---- Fuzzy matching ------------------------------------------------------------------------

// How well a query lines up with a title. Lower ranks first.
enum class Class : std::uint8_t {
    Prefix,       // The title starts with the query (`tog` in `Toggle panel`).
    WordPrefix,   // A word of the title starts with the query (`pan`).
    WordBoundary, // Every run of matched characters starts a word (`tp`).
    Fuzzy,        // Any in-order subsequence (`gl`).
};

// A successful match; `indices` are ascending char (code point, not byte) indices into the title.
struct Match {
    Class cls = Class::Prefix;
    std::int32_t score = 0;
    std::vector<std::size_t> indices;
    friend bool operator==(const Match&, const Match&) = default;
};

// The query's characters appear in the title in order, ignoring case. Reusable; keeps scratch
// buffers so filtering on each keystroke does not allocate.
class Matcher {
  public:
    // Sets the query; whitespace is ignored, so `tog pan` and `togpan` behave alike.
    void set_query(std::string_view query);
    // Whether the query is empty (every title matches).
    [[nodiscard]] bool is_empty() const { return query_.empty(); }
    [[nodiscard]] std::optional<Match> matches(std::string_view title);

  private:
    [[nodiscard]] std::int32_t base(std::size_t j) const;
    [[nodiscard]] std::int32_t score_of(std::span<const std::size_t> indices) const;
    std::optional<std::pair<std::int32_t, std::vector<std::size_t>>> align(bool word_runs);

    std::vector<char32_t> query_;
    std::vector<char32_t> title_;
    std::vector<bool> boundary_;
    std::vector<std::int32_t> score_;
    std::vector<std::size_t> parent_;
};

// A byte range of a title and whether it is matched.
struct Run {
    std::size_t begin = 0, end = 0;
    bool matched = false;
    friend bool operator==(const Run&, const Run&) = default;
};

// Byte ranges of `title` flagged true where matched, for highlighting `indices`.
[[nodiscard]] std::vector<Run> highlight_runs(std::string_view title, std::span<const std::size_t> indices);

// Indices of `candidates` matching `query`, best first (class, score, shorter, earlier), at
// most `limit`. An empty query keeps the original order.
[[nodiscard]] std::vector<std::size_t> filter(std::string_view query, std::span<const std::string_view> candidates, std::size_t limit);

// ---- Formatting ----------------------------------------------------------------------------

// "0.4s", "12s", "2m 5s", "1h 3m".
[[nodiscard]] std::string format_duration(std::int64_t ms);
// "512 B", "1.5 KB", "3.0 MB", "2.1 GB".
[[nodiscard]] std::string format_bytes(std::uint64_t bytes);
// `s` cut to at most `max` chars, ending in `…` when cut.
[[nodiscard]] std::string truncate_trailing(std::string_view s, std::size_t max);
// `s` cut to its last `max` chars, starting with `…` when cut (paths keep their file name).
[[nodiscard]] std::string truncate_leading(std::string_view s, std::size_t max);

// ---- Ellipsis ------------------------------------------------------------------------------

// The single-character ellipsis appended or inserted when text is truncated.
inline constexpr std::string_view ELLIPSIS = "\u2026";

// How overflowing single-line text is shortened.
enum class EllipsisMode : std::uint8_t {
    End,  // Keep as much of the start as fits and append `…`.
    Path, // Preserve the last path component (`/` or `\`), eliding parent segments.
};

using Measure = std::function<float(std::string_view)>;

// Shortens `text` to fit within `max_width` using a caller-supplied width `measure`.
[[nodiscard]] std::string ellipsize_with(std::string_view text, float max_width, EllipsisMode mode, const Measure& measure);

// The single-line rendered width of `content` at `size` pixels in `font` (needs a `QGuiApplication`).
[[nodiscard]] float measure_text_width(std::string_view content, float size, theme::Font font);

// Shortens `content` to fit within `max_width` using real font measurement.
[[nodiscard]] std::string ellipsize_text(std::string_view content, float max_width, float size, theme::Font font, EllipsisMode mode);

} // namespace loa::ui::kit::text
