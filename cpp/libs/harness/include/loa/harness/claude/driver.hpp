#pragma once

#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/harness/kit/rpc.hpp"

namespace loa::harness::claude {
inline constexpr std::string_view HARNESS = "claude-code";
inline constexpr std::chrono::seconds EXIT_GRACE{10};

[[nodiscard]] std::vector<std::string> command_args(const loa::contracts::SessionConfig& config,
                                                    const std::vector<std::string>& resume = {});
[[nodiscard]] loa::contracts::Result<std::vector<std::string>> resume_args(const loa::core::Dom& dom, bool fork);
[[nodiscard]] std::optional<std::string> fork_point(const loa::core::Dom& dom);

class ClaudeControl final : public kit::Framing {
  public:
    [[nodiscard]] kit::Json request(const kit::Json& id, std::string_view method, kit::Json params) const override;
    [[nodiscard]] kit::Json notification(std::string_view method, kit::Json params) const override;
    [[nodiscard]] kit::Json response(kit::Json id, std::string_view method, kit::RpcResult<kit::Json> result) const override;
    [[nodiscard]] std::optional<kit::Incoming> classify(kit::Json msg) const override;
};

class ClaudeAdapter final : public loa::contracts::Adapter {
  public:
    ClaudeAdapter() : executable_("claude") {}
    explicit ClaudeAdapter(std::filesystem::path executable) : executable_(std::move(executable)) {}
    [[nodiscard]] static ClaudeAdapter with_executable(std::filesystem::path executable) { return ClaudeAdapter(std::move(executable)); }
    [[nodiscard]] std::string_view harness() const override { return HARNESS; }
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Availability> availability() const override;
    [[nodiscard]] loa::contracts::Capabilities capabilities() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>> start(
        loa::contracts::SessionConfig config, std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>> resume(
        loa::contracts::SessionConfig config, const loa::core::Dom& dom,
        std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>> fork(
        loa::contracts::SessionConfig config, const loa::core::Dom& dom,
        std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Result<> import(const std::filesystem::path& path, loa::core::PatchSink& sink) const override;
    [[nodiscard]] loa::contracts::Result<> export_(const loa::core::Dom& dom, const std::filesystem::path& out) const override;

  private:
    std::filesystem::path executable_;
};
} // namespace loa::harness::claude
