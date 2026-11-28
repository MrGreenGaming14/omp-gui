#pragma once

#include <iosfwd>
#include <string_view>

#include "loa/contracts/harness.hpp"

namespace loa::harness::codex::rollout {

// Import a Codex JSONL rollout into an empty sink; node identifies its owner.
[[nodiscard]] loa::contracts::Result<> import(std::istream& input, loa::core::PatchSink& sink, std::string_view node);
// Export a session DOM as Codex JSONL, preserving imported lines and extensions.
[[nodiscard]] loa::contracts::Result<> export_(const loa::core::Dom& dom, std::ostream& output);

// Native codec entry points used by the adapter.
struct CodexNative {
    [[nodiscard]] static loa::contracts::Result<> import(std::istream& input, loa::core::PatchSink& sink,
                                                          std::string_view node);
    [[nodiscard]] static loa::contracts::Result<> export_(const loa::core::Dom& dom, std::ostream& output);
};

} // namespace loa::harness::codex::rollout
