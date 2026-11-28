#pragma once

// One model call through `omp -p` (port of `OneShot` in `omp/mod.rs`).

#include <chrono>
#include <expected>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/harness.hpp"

namespace loa::harness::omp {

// `--config` overlay that switches omp's memory off for the run. `"off"` is quoted: a
// YAML 1.1 parser reads a bare `off` as the boolean `false`.
inline constexpr std::string_view MEMORY_OFF = "memory:\n  backend: \"off\"\n";

// The prompt goes in on stdin, the answer is the printed text. The run uses a model role
// (`@smol` unless set), saves no session, loads no tools, context files, rules, skills,
// extensions or language servers, and turns omp's memory off.
class OneShot final : public loa::contracts::Summarizer {
  public:
    // `executable` is the omp binary, `cwd` the directory it starts in.
    OneShot(std::filesystem::path executable, std::filesystem::path cwd)
        : executable_(std::move(executable)), cwd_(std::move(cwd)) {}

    // Model or role passed to `--model`, e.g. `@smol` or `provider/id`.
    OneShot& with_model(std::string model) & { model_ = std::move(model); return *this; }
    OneShot&& with_model(std::string model) && { model_ = std::move(model); return std::move(*this); }
    OneShot& with_timeout(std::chrono::milliseconds timeout) & { timeout_ = timeout; return *this; }
    OneShot&& with_timeout(std::chrono::milliseconds timeout) && { timeout_ = timeout; return std::move(*this); }

    // `model` trimmed when it is set and non-empty, else the configured model.
    [[nodiscard]] std::string_view pick(std::optional<std::string_view> model) const;
    // The command-line arguments for one run.
    [[nodiscard]] std::vector<std::string> args(std::string_view model, const std::filesystem::path& overlay) const;

    // Runs `prompt` on `model` and returns the printed answer.
    [[nodiscard]] loa::contracts::Awaitable<std::expected<std::string, std::string>> ask(std::string model, std::string prompt) const;
    [[nodiscard]] loa::contracts::Awaitable<std::expected<std::string, std::string>> summarize(std::string instructions,
                                                                                             std::string input) const override;

  private:
    std::filesystem::path executable_;
    std::filesystem::path cwd_;
    std::string model_ = "@smol";
    std::chrono::milliseconds timeout_ = std::chrono::seconds(180);
};

} // namespace loa::harness::omp
