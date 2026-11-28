#pragma once

// Private: shared by the omp native import and export.

#include <string>
#include <string_view>

#include "loa/contracts/harness.hpp"
#include "loa/core/dom.hpp"
#include "loa/core/sink.hpp"
#include "loa/harness/native.hpp"
#include "loa/runtime/json.hpp"

namespace loa::harness::omp::native::detail {

inline constexpr std::string_view HARNESS = loa::harness::native::OMP;
// The exact JSON line of the entry an element stands for.
inline constexpr std::string_view X_ENTRY = "x-omp-entry";
// The position of that line among the file's non-blank, valid lines.
inline constexpr std::string_view X_LINE = "x-omp-line";

// One compact JSON line, as serde_json writes it.
[[nodiscard]] inline std::string to_line(const loa::runtime::json::Value& value) { return loa::runtime::json::dump(value); }

[[nodiscard]] loa::contracts::Result<> run_import(std::string_view text, loa::core::PatchSink& sink, std::string_view node);
[[nodiscard]] loa::contracts::Result<std::string> run_export(const loa::core::Dom& dom);

} // namespace loa::harness::omp::native::detail
