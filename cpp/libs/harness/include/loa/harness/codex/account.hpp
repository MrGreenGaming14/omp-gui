#pragma once

#include <filesystem>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/account.hpp"
#include "loa/harness/account_support/process.hpp"
#include "loa/settings/models/error.hpp"

namespace loa::harness::codex {

// Codex CLI account and its user-level config.toml settings.
enum class ReasoningEffort { None, Minimal, Low, Medium, High, XHigh };

[[nodiscard]] std::optional<ReasoningEffort> parse_reasoning_effort(std::string_view value);
[[nodiscard]] std::string_view reasoning_effort_name(ReasoningEffort effort);

struct CodexModelSettings {
    std::optional<std::string> model;
    std::optional<ReasoningEffort> reasoning_effort;
    friend bool operator==(const CodexModelSettings&, const CodexModelSettings&) = default;
};

class Codex final : public loa::contracts::HarnessAccount {
  public:
    Codex(std::filesystem::path program, std::filesystem::path config_path,
          std::shared_ptr<const account_support::ProcessRunner> runner);
    // Resolve `$CODEX_HOME/config.toml`, falling back to `$HOME/.codex/config.toml`.
    Codex(std::filesystem::path program, std::shared_ptr<const account_support::ProcessRunner> runner);

    [[nodiscard]] std::string_view id() const override;
    [[nodiscard]] std::string_view name() const override;
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::SignInState> sign_in_state() const override;
    [[nodiscard]] loa::contracts::Result<std::unique_ptr<loa::contracts::LoginStrategy>>
    login(loa::contracts::LoginKind kind) const override;
    [[nodiscard]] std::optional<loa::contracts::CommandSpec> logout_command() const override;
    [[nodiscard]] std::span<const std::string_view> efforts() const override;
    [[nodiscard]] std::optional<std::filesystem::path> config_path() const override;
    [[nodiscard]] loa::contracts::Result<loa::contracts::ModelSettings> read_model() const override;
    [[nodiscard]] loa::contracts::Result<> write_model(const loa::contracts::ModelSettings& settings) const override;

    [[nodiscard]] loa::settings::models::Result<CodexModelSettings> read_model_settings() const;
    [[nodiscard]] loa::settings::models::Result<> set_model(std::optional<std::string_view> model) const;
    [[nodiscard]] loa::settings::models::Result<> set_reasoning_effort(std::optional<ReasoningEffort> effort) const;

  private:
    [[nodiscard]] loa::contracts::CommandSpec command(std::vector<std::string> args) const;
    [[nodiscard]] loa::settings::models::Result<> edit(std::string_view key, std::optional<std::string_view> setting) const;

    std::filesystem::path program_;
    std::filesystem::path config_path_;
    std::shared_ptr<const account_support::ProcessRunner> runner_;
};

} // namespace loa::harness::codex
