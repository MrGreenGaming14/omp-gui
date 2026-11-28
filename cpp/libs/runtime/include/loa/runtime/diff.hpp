#pragma once

// Myers line and word diffs, op-for-op identical to `similar` 3.2
// (`TextDiff::configure().algorithm(Algorithm::Myers)` with no deadline).

#include <cstddef>
#include <cstdint>
#include <span>
#include <string_view>
#include <vector>

namespace loa::runtime::diff {

enum class Tag : std::uint8_t { equal, delete_, insert, replace };

// One diff op over token indices. `equal` has `old_len == new_len`, `delete_` has
// `new_len == 0` and `insert` has `old_len == 0`.
struct Op {
    Tag tag = Tag::equal;
    std::size_t old_index = 0;
    std::size_t old_len = 0;
    std::size_t new_index = 0;
    std::size_t new_len = 0;

    [[nodiscard]] std::size_t old_end() const noexcept { return old_index + old_len; }
    [[nodiscard]] std::size_t new_end() const noexcept { return new_index + new_len; }
    friend bool operator==(const Op&, const Op&) = default;
};

// Ops turning `old` into `new`: Myers with similar's preflight, heuristics, compaction
// and replace grouping.
[[nodiscard]] std::vector<Op> diff_slices(std::span<const std::uint32_t> old, std::span<const std::uint32_t> new_);

// A tokenized diff; the tokens view the input strings.
struct TextDiff {
    std::vector<std::string_view> old_tokens;
    std::vector<std::string_view> new_tokens;
    std::vector<Op> ops;

    // Similarity in [0, 1]: twice the equal tokens over all tokens (1 when both are empty).
    [[nodiscard]] float ratio() const noexcept;
};

// Tokens are lines including their `\n`, `\r\n` or `\r` terminator.
[[nodiscard]] TextDiff diff_lines(std::string_view old, std::string_view new_);
// Tokens are maximal runs of whitespace and of non-whitespace (Unicode White_Space).
[[nodiscard]] TextDiff diff_words(std::string_view old, std::string_view new_);

} // namespace loa::runtime::diff
