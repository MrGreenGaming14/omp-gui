#pragma once

// Lossless round trips of native records: the fields the DOM models are taken out, the
// rest is kept as residual JSON on the node and merged back on export. Multi-part text is
// joined for the DOM with its part lengths recorded, so export can split it again.

#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/core/dom.hpp"
#include "loa/core/value.hpp"
#include "loa/runtime/json.hpp"

namespace loa::harness::kit::residual {

// The attribute holding `harness`'s residual JSON.
[[nodiscard]] std::string attr_name(std::string_view harness);

// Moves every field of `obj` not named in `keep` into the returned residual.
[[nodiscard]] loa::runtime::json::Object take(loa::runtime::json::Object& obj, std::span<const std::string_view> keep);

// The residual `harness` stored on `node`; empty when none or malformed.
[[nodiscard]] loa::runtime::json::Object restore(const loa::core::Node& node, std::string_view harness);

// Joins `parts` with `separator`; the lengths let `split_parts` undo it.
[[nodiscard]] std::pair<std::string, std::vector<loa::core::Value>> join_parts(std::span<const std::string_view> parts,
                                                                               std::string_view separator);

// Splits text `join_parts` produced; `nullopt` when it was edited since.
[[nodiscard]] std::optional<std::vector<std::string_view>> split_parts(std::string_view text, std::string_view separator,
                                                                       std::span<const loa::core::Value> lengths);

} // namespace loa::harness::kit::residual
