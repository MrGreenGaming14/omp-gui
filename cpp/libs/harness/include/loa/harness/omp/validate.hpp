#pragma once

#include <filesystem>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

#include "loa/harness/account_support/process.hpp"
#include "loa/settings/models/error.hpp"

namespace loa::harness::omp {

// Validate candidate config and model files in a fresh, isolated agent directory.
class OmpSettingsValidator final : public settings::models::SettingsValidator {
  public:
    OmpSettingsValidator(std::filesystem::path program, std::shared_ptr<const account_support::ProcessRunner> runner)
        : program(std::move(program)), runner(std::move(runner)) {}
    std::filesystem::path program;
    std::shared_ptr<const account_support::ProcessRunner> runner;
    [[nodiscard]] contracts::Awaitable<settings::models::Result<>> check_config(
        std::string text, std::vector<std::pair<std::string, runtime::json::Value>> expected) const override;
    [[nodiscard]] contracts::Awaitable<settings::models::Result<>> check_models(std::string text) const override;
  private:
    [[nodiscard]] contracts::Awaitable<settings::models::Result<std::string>> command(
        std::string text, std::string_view file, std::vector<std::string> args) const;
};

} // namespace loa::harness::omp
