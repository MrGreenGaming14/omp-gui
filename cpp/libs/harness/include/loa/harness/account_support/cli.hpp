#pragma once

// Driving a harness CLI login without a terminal (port of `account_support/cli.rs`).

#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/contracts/account.hpp"
#include "loa/harness/account_support/process.hpp"

namespace loa::harness::account_support {

// Starts a CLI login when its strategy is run.
class CliLoginCommand final : public loa::contracts::LoginStrategy {
  public:
    CliLoginCommand(std::shared_ptr<const ProcessRunner> runner, CommandSpec command)
        : runner_(std::move(runner)), command_(std::move(command)) {}

    [[nodiscard]] Awaitable<void> run(std::shared_ptr<loa::contracts::LoginChannels> channels) override;

  private:
    std::shared_ptr<const ProcessRunner> runner_;
    CommandSpec command_;
};

// A started CLI login process. Lines it prints become `Progress` updates after escape
// sequences and carriage returns are removed; the first web URL becomes a `Url` update and
// an unterminated line ending in `:`, `?` or `>` a `Prompt`. Asio channels aren't
// thread-safe: run it on a strand (or single-threaded executor) shared with the channels.
class CliLogin final : public loa::contracts::LoginStrategy {
  public:
    explicit CliLogin(std::unique_ptr<RunningProcess> process) : process_(std::move(process)) {}

    [[nodiscard]] Awaitable<void> run(std::shared_ptr<loa::contracts::LoginChannels> channels) override;

  private:
    std::unique_ptr<RunningProcess> process_;
};

namespace cli {

// The first `http://` or `https://` URL in `text`, without trailing `.`, `,` or `;`.
[[nodiscard]] std::optional<std::string_view> first_url(std::string_view text);

// Removes ANSI escape sequences, carriage returns and control characters, then trims.
[[nodiscard]] std::string clean(std::string_view text);

struct Text {
    std::string text;
    friend bool operator==(const Text&, const Text&) = default;
};
struct Prompt {
    std::string message;
    friend bool operator==(const Prompt&, const Prompt&) = default;
};
using Line = std::variant<Text, Prompt>;

// Splits output into cleaned lines and reports an unterminated last line that ends like a
// question as a prompt.
class LineSplitter {
  public:
    [[nodiscard]] std::vector<Line> push(std::string_view chunk);
    [[nodiscard]] std::vector<Line> finish();

  private:
    std::string partial_;
};

} // namespace cli

} // namespace loa::harness::account_support
