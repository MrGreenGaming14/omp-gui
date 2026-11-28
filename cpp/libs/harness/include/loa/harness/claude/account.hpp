#pragma once

#include <filesystem>
#include <map>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/account.hpp"
#include "loa/harness/account_support/configdoc.hpp"
#include "loa/harness/account_support/process.hpp"
#include "loa/settings/models/error.hpp"

namespace loa::harness::claude {

// Effort values accepted by Claude Code's user settings.json.
enum class ClaudeEffort { Low, Medium, High, XHigh };
[[nodiscard]] std::optional<ClaudeEffort> parse_effort(std::string_view value) noexcept;
[[nodiscard]] std::string_view effort_name(ClaudeEffort effort) noexcept;

struct ClaudeModelSettings {
    std::optional<std::string> model;
    std::optional<ClaudeEffort> effort;
    std::map<std::string, ClaudeEffort> per_model_effort;
    friend bool operator==(const ClaudeModelSettings&, const ClaudeModelSettings&) = default;
};

// Claude Code account, with its CLI and user settings.json (normally ~/.claude/settings.json).
class ClaudeCode final : public loa::contracts::HarnessAccount {
  public:
    ClaudeCode(std::filesystem::path program, std::filesystem::path settings_path,
               std::shared_ptr<const account_support::ProcessRunner> runner)
        : program_(std::move(program)), settings_path_(std::move(settings_path)), runner_(std::move(runner)) {}

    [[nodiscard]] const std::filesystem::path& program() const noexcept { return program_; }
    [[nodiscard]] const std::filesystem::path& settings_path() const noexcept { return settings_path_; }
    [[nodiscard]] account_support::Result<ClaudeModelSettings> read_model_settings() const;
    [[nodiscard]] account_support::Result<> set_model(std::optional<std::string_view> model) const;
    [[nodiscard]] account_support::Result<> set_effort(std::optional<ClaudeEffort> effort) const;
    [[nodiscard]] account_support::Result<> set_model_effort(std::string_view model, std::optional<ClaudeEffort> effort) const;

    [[nodiscard]] std::string_view id() const override;
    [[nodiscard]] std::string_view name() const override { return "Claude Code"; }
    [[nodiscard]] loa::contracts::Awaitable<loa::contracts::SignInState> sign_in_state() const override;
    [[nodiscard]] loa::contracts::Result<std::unique_ptr<loa::contracts::LoginStrategy>> login(loa::contracts::LoginKind kind) const override;
    [[nodiscard]] std::optional<loa::contracts::CommandSpec> logout_command() const override;
    [[nodiscard]] std::span<const std::string_view> efforts() const override;
    [[nodiscard]] std::optional<std::filesystem::path> config_path() const override { return settings_path_; }
    [[nodiscard]] loa::contracts::Result<loa::contracts::ModelSettings> read_model() const override;
    [[nodiscard]] loa::contracts::Result<> write_model(const loa::contracts::ModelSettings& settings) const override;

  private:
    [[nodiscard]] loa::contracts::CommandSpec command(std::vector<std::string> args) const;
    template <typename Change>
    [[nodiscard]] account_support::Result<> edit(Change&& change) const {
        auto loaded = loa::settings::models::load_snapshot(settings_path_);
        if (!loaded) return std::unexpected(std::move(loaded.error()));
        auto& [snapshot, text] = *loaded;
        auto edited = account_support::edit_file<account_support::JsonDoc>(
            snapshot, text ? std::string_view(*text) : std::string_view{}, std::forward<Change>(change));
        if (!edited) return std::unexpected(std::move(edited.error()));
        return {};
    }

    std::filesystem::path program_;
    std::filesystem::path settings_path_;
    std::shared_ptr<const account_support::ProcessRunner> runner_;
};

} // namespace loa::harness::claude
