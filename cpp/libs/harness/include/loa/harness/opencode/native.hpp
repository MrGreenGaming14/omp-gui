#pragma once

// OpenCode's `opencode export` JSON document: import feeds the same mapper as live events;
// export restores native fields kept on the DOM as x-oc attributes and ext elements.

#include <filesystem>
#include <vector>

#include "loa/harness/kit/native.hpp"
#include "loa/runtime/json.hpp"

namespace loa::harness::opencode {

[[nodiscard]] loa::contracts::Result<loa::runtime::json::Value> export_value(const loa::core::Dom& dom);

class OpenCodeNative final : public loa::harness::kit::NativeCodec {
  public:
    [[nodiscard]] loa::contracts::Result<> import(const std::filesystem::path& path,
                                                  loa::core::PatchSink& sink) const override;
    [[nodiscard]] loa::contracts::Result<> export_(const loa::core::Dom& dom,
                                                   const std::filesystem::path& out) const override;
    [[nodiscard]] std::vector<loa::harness::kit::NativeSession> discover(const std::filesystem::path& home) const override;
};

} // namespace loa::harness::opencode
