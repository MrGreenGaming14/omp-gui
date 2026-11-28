#pragma once

// Where the Usage window's data comes from (the `UsageSource` port of
// `loa-app/src/app/usage.rs`), its adapter over the `omp usage` command, and what
// `omp usage --json` reports. The window's state is in `loa/app/usage_window.hpp`.

#include <cstdint>
#include <expected>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/view/shell.hpp"

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

namespace usage {

// One limit window of an account, e.g. "Claude 7 Day" at 51%.
struct Limit {
    std::string label;
    // Short window name for the meter, e.g. "7d".
    std::string window;
    // Used share, 0 to 1; `nullopt` when the provider did not report it.
    std::optional<float> fraction;
    // Unix milliseconds of the next reset.
    std::optional<std::int64_t> resets_at;
    friend bool operator==(const Limit&, const Limit&) = default;
};

// One signed-in account of a provider.
struct Account {
    std::string label;
    // `<provider>/<credential id>`, what `omp usage reset` takes.
    std::optional<std::string> reset_target;
    std::vector<Limit> limits;
    // Saved resets left, and how many of them can be spent now.
    std::pair<std::uint64_t, std::uint64_t> resets{0, 0};
    friend bool operator==(const Account&, const Account&) = default;
};

struct Provider {
    std::string id;
    std::string name;
    std::vector<Account> accounts;
    // omp's capacity line, e.g. "5h 0.11 of 2 used · 7d 0.71 of 2 used".
    std::optional<std::string> capacity;

    // The highest used share across this provider's limits.
    [[nodiscard]] std::optional<float> peak() const;
    friend bool operator==(const Provider&, const Provider&) = default;
};

struct Usage {
    std::int64_t generated_at = 0;
    std::vector<Provider> providers;

    // The sidebar meter: the tightest limit across every provider.
    [[nodiscard]] std::optional<loa::view::shell::UsageMeter> meter() const;
    friend bool operator==(const Usage&, const Usage&) = default;
};

// Reads `omp usage --json`.
[[nodiscard]] std::expected<Usage, std::string> parse(std::string_view json);

} // namespace usage

} // namespace loa::app
