#pragma once

#include <filesystem>
#include <vector>

#include "loa/harness/kit/native.hpp"

namespace loa::harness::claude::native {

// Claude Code .jsonl transcripts and their side directories.
[[nodiscard]] loa::contracts::Result<> import(const std::filesystem::path& path, loa::core::PatchSink& sink);
[[nodiscard]] loa::contracts::Result<> export_(const loa::core::Dom& dom, const std::filesystem::path& out);
[[nodiscard]] std::vector<kit::NativeSession> discover(const std::filesystem::path& home);

class ClaudeNative final : public kit::NativeCodec {
  public:
    [[nodiscard]] loa::contracts::Result<> import(const std::filesystem::path& path, loa::core::PatchSink& sink) const override;
    [[nodiscard]] loa::contracts::Result<> export_(const loa::core::Dom& dom, const std::filesystem::path& out) const override;
    [[nodiscard]] std::vector<kit::NativeSession> discover(const std::filesystem::path& home) const override;
};

} // namespace loa::harness::claude::native
