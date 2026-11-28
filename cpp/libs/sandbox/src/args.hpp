#pragma once

// Internal: serde-style `deny_unknown_fields` and u32 checks for argument decoding.

#include <cstdint>
#include <expected>
#include <format>
#include <initializer_list>
#include <optional>
#include <string>
#include <string_view>

#include "loa/contracts/tools.hpp"

namespace loa::sandbox::detail {

// `invalid type` unless `json` is an object, then serde's `unknown field` error for a key not in `known`.
inline std::expected<void, std::string> fields(const contracts::Json& json, std::string_view what,
                                               std::initializer_list<std::string_view> known) {
    if (auto ok = contracts::args::object(json, what); !ok) return ok;
    for (const auto& [key, _] : *json.as_object()) {
        bool found = false;
        for (const auto k : known) found |= k == key;
        if (found) continue;
        std::string expected;
        std::size_t i = 0;
        for (const auto k : known) {
            if (i > 0) expected += known.size() == 2 ? " or " : ", ";
            expected += std::format("`{}`", k);
            ++i;
        }
        if (known.size() == 0) return std::unexpected(std::format("unknown field `{}`, there are no fields", key));
        if (known.size() > 2) expected = "one of " + expected;
        return std::unexpected(std::format("unknown field `{}`, expected {}", key, expected));
    }
    return {};
}

// An optional u32 field.
inline std::expected<std::optional<std::uint32_t>, std::string> optional_u32(const contracts::Json& json, std::string_view name) {
    auto v = contracts::args::optional_u64(json, name);
    if (!v) return std::unexpected(v.error());
    if (!*v) return std::optional<std::uint32_t>{};
    if (**v > 0xffffffffu) return std::unexpected(std::format("invalid value: integer `{}`, expected u32", **v));
    return std::optional<std::uint32_t>(static_cast<std::uint32_t>(**v));
}

} // namespace loa::sandbox::detail
