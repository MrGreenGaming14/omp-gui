#pragma once

// The omp-backed gate ports of the updater (port of `loa-app/src/adapters/update_gates.rs`).

#include <chrono>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/runtime/json.hpp"
#include "loa/updater/gates.hpp"

namespace loa::runtime {
class Executor;
}

namespace loa::app::adapters {

// Plugs the omp probes and the smol reviewer into the updater.
class OmpPorts final : public loa::updater::GatePorts {
  public:
    std::unique_ptr<loa::updater::Probe> probe(const loa::updater::OmpGates& gates, std::chrono::milliseconds timeout) override;
    std::unique_ptr<loa::updater::ChangelogSummarizer> reviewer(const std::filesystem::path& reviewer, const std::string& model,
                                                                std::chrono::milliseconds timeout) override;
};

// Reviews changelogs through a one-shot model call and parses the reply strictly.
class SmolReviewer final : public loa::updater::ChangelogSummarizer {
  public:
    explicit SmolReviewer(std::unique_ptr<const loa::contracts::Summarizer> summarizer);
    ~SmolReviewer() override;
    loa::updater::PortResult<loa::updater::ChangelogReview> review(const std::string& changelog) override;

  private:
    std::unique_ptr<const loa::contracts::Summarizer> summarizer_;
    std::unique_ptr<loa::runtime::Executor> exec_;
};

// Reads tool schemas, session format, config keys, RPC versions and golden round trips from an omp.
class OmpProbe final : public loa::updater::Probe {
  public:
    // `tools`: loaded when listing schemas; tools a version lacks are left out of its list.
    // `timeout`: limit for one probe, process start included.
    OmpProbe(std::vector<std::string> tools, std::chrono::milliseconds timeout);
    ~OmpProbe() override;

    loa::updater::PortResult<std::vector<loa::updater::ToolSchema>> tools(const std::filesystem::path& executable) override;
    loa::updater::PortResult<std::uint32_t> session_version(const std::filesystem::path& executable) override;
    loa::updater::PortResult<loa::updater::ConfigSchema> config_keys(const std::filesystem::path& executable) override;
    loa::updater::PortResult<std::vector<std::uint32_t>> protocol_versions(const std::filesystem::path& executable) override;
    loa::updater::PortResult<std::vector<loa::updater::ContractFailure>> run(const std::filesystem::path& executable) override;

  private:
    std::vector<std::string> tools_;
    std::chrono::milliseconds timeout_;
    std::unique_ptr<loa::runtime::Executor> exec_;
};

// Pure helpers, exposed for tests.
namespace detail {

using Json = loa::runtime::json::Value;

struct Golden {
    std::string_view name;
    std::string_view jsonl;
};
// Golden omp sessions (JSONL) the app must keep reading the same way: `basic`, `branched`.
[[nodiscard]] std::span<const Golden> goldens();

// The tool omp names when refusing `--tools`.
[[nodiscard]] std::optional<std::string> refused_tool(std::string_view stderr_text);
// Keys of `config list --json`; `record` keys are open.
[[nodiscard]] std::expected<loa::updater::ConfigSchema, std::string> parse_config_list(std::string_view stdout_text);
[[nodiscard]] std::expected<std::vector<Json>, std::string> parse_lines(std::string_view text);
[[nodiscard]] std::optional<std::string_view> entry_id(const Json& entry);
// Tree entries: every line with an id but the header and title.
[[nodiscard]] bool is_entry(const Json& line);
[[nodiscard]] std::expected<std::vector<Json>, std::string> entries(std::string_view jsonl);
// The golden text with its header's `cwd` set to `dir`.
[[nodiscard]] std::expected<std::string, std::string> with_cwd(std::string_view jsonl, const std::filesystem::path& dir);
// Entries from the root to `leaf`, root first.
[[nodiscard]] std::vector<const Json*> branch(const std::vector<Json>& entries, std::optional<std::string_view> leaf);
// Differences between expected and actual entries, by id: added fields pass; missing, changed and extra entries don't.
[[nodiscard]] std::vector<std::string> diff_entries(const std::vector<Json>& expected, const std::vector<Json>& actual);

} // namespace detail

} // namespace loa::app::adapters
