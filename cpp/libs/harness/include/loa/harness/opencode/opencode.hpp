#pragma once

#include <chrono>
#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/runtime/json.hpp"
#include "loa/harness/opencode/client.hpp"

namespace loa::harness::opencode {

inline constexpr std::string_view DEFAULT_URL = "http://127.0.0.1:4096";
inline constexpr std::chrono::seconds PROBE_TIMEOUT{2};

[[nodiscard]] std::vector<loa::contracts::HarnessModel> provider_models(const loa::runtime::json::Value& providers);

class OpenCodeSession final : public loa::contracts::HarnessSession {
  public:
    struct Impl;
    explicit OpenCodeSession(std::unique_ptr<Impl> impl);
    ~OpenCodeSession() override;
    OpenCodeSession(const OpenCodeSession&) = delete;
    OpenCodeSession& operator=(const OpenCodeSession&) = delete;

    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> send(loa::contracts::Prompt prompt) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> interrupt() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> answer(loa::core::NodeId decision, loa::contracts::Answer answer) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> set_model(loa::contracts::ModelSelection selection) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<std::vector<loa::contracts::HarnessModel>>> models() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> set_mode(loa::core::SessionMode mode) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> compact(std::optional<std::string> instructions) const override;
    [[nodiscard]] std::string_view harness() const override { return "opencode"; }
    [[nodiscard]] loa::contracts::SessionState state() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> wait_idle() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<>> shutdown() const override;

  private:
    std::unique_ptr<Impl> impl_;
};

class OpenCodeAdapter final : public loa::contracts::Adapter {
  public:
    explicit OpenCodeAdapter(std::string base = std::string(DEFAULT_URL), std::optional<ServerAuth> auth = std::nullopt);
    [[nodiscard]] const std::string& base() const noexcept { return base_; }
    [[nodiscard]] std::string_view harness() const override { return "opencode"; }
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Availability> availability() const override;
    [[nodiscard]] loa::contracts::Capabilities capabilities() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>> start(
        loa::contracts::SessionConfig config, std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>> resume(
        loa::contracts::SessionConfig config, const loa::core::Dom& dom, std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::Result<loa::contracts::SessionPtr>> fork(
        loa::contracts::SessionConfig config, const loa::core::Dom& dom, std::shared_ptr<loa::core::PatchSink> sink) const override;
    [[nodiscard]] loa::contracts::Result<> import(const std::filesystem::path& path, loa::core::PatchSink& sink) const override;
    [[nodiscard]] loa::contracts::Result<> export_(const loa::core::Dom& dom, const std::filesystem::path& path) const override;

  private:
    std::string base_;
    std::optional<ServerAuth> auth_;
};

void register_factory();

} // namespace loa::harness::opencode

namespace loa::harness {
using opencode::OpenCodeAdapter;
using opencode::OpenCodeSession;
} // namespace loa::harness
