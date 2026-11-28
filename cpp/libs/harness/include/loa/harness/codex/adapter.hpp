#pragma once

#include <filesystem>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/harness.hpp"

namespace loa::harness::codex {

inline constexpr std::string_view HARNESS = "codex";

// Drives `codex app-server` and converts Codex rollout files.
class CodexAdapter final : public loa::contracts::Adapter {
  public:
    explicit CodexAdapter(std::string node);
    [[nodiscard]] CodexAdapter with_executable(std::filesystem::path executable) const;
    [[nodiscard]] CodexAdapter with_args(std::vector<std::string> args) const;

    [[nodiscard]] std::string_view harness() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Availability> availability() const override;
    [[nodiscard]] loa::contracts::Capabilities capabilities() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>>
    start(loa::contracts::SessionConfig config, std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>>
    resume(loa::contracts::SessionConfig config, const loa::core::Dom& dom,
           std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>>
    fork(loa::contracts::SessionConfig config, const loa::core::Dom& dom,
         std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Result<> import(const std::filesystem::path& native,
                                                   loa::core::PatchSink& sink) const override;
    [[nodiscard]] loa::contracts::Result<> export_(const loa::core::Dom& dom,
                                                    const std::filesystem::path& out) const override;

    [[nodiscard]] const std::filesystem::path& executable() const noexcept { return executable_; }
    [[nodiscard]] const std::vector<std::string>& args() const noexcept { return args_; }

  private:
    std::filesystem::path executable_ = "codex";
    std::vector<std::string> args_;
    std::string node_;
};

// Registers the Codex adapter and account with the harness factory registry.
void register_factory();

} // namespace loa::harness::codex
