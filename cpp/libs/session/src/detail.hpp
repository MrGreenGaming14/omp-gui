#pragma once

// Helpers shared by the session sources; not part of the public API.

#include <cstdint>
#include <initializer_list>
#include <string_view>
#include <utility>

#include "loa/core/sink.hpp"
#include "loa/core/value.hpp"
#include "loa/session/error.hpp"

namespace loa::session::detail {

[[nodiscard]] core::Attrs attrs(std::initializer_list<std::pair<std::string_view, core::Value>> pairs);
// `seq` as an `Int`, saturated at `i64::MAX`.
[[nodiscard]] core::Value seq_value(std::uint64_t seq);
[[nodiscard]] Result<core::Patch> commit(core::PatchSink& sink, core::Draft draft);

[[nodiscard]] constexpr bool is_space(char c) noexcept {
    return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v';
}

[[nodiscard]] constexpr std::string_view trim(std::string_view text) noexcept {
    while (!text.empty() && is_space(text.front())) text.remove_prefix(1);
    while (!text.empty() && is_space(text.back())) text.remove_suffix(1);
    return text;
}

} // namespace loa::session::detail
