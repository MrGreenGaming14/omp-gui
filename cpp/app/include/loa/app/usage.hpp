#pragma once

// Where the Usage window's data comes from (the `UsageSource` port of
// `loa-app/src/app/usage.rs`) and its adapter over the `omp usage` command. The window itself
// and the parsing of `omp usage --json` belong to the usage module.

#include <expected>
#include <string>

#include "loa/contracts/async.hpp"

namespace loa::app {

class UsageSource {
  public:
    virtual ~UsageSource() = default;
    // `omp usage --json` output, or why it couldn't be read.
    [[nodiscard]] virtual contracts::Awaitable<std::expected<std::string, std::string>> fetch() const = 0;
    // Spends a saved reset of account `target` (`<provider>/<credential id>`).
    [[nodiscard]] virtual contracts::Awaitable<std::expected<void, std::string>> spend(std::string target) const = 0;
};

// `UsageSource` over the `omp usage` command.
class OmpUsage final : public UsageSource {
  public:
    [[nodiscard]] contracts::Awaitable<std::expected<std::string, std::string>> fetch() const override;
    [[nodiscard]] contracts::Awaitable<std::expected<void, std::string>> spend(std::string target) const override;
};

} // namespace loa::app
