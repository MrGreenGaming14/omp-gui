#pragma once

// Gate checks a staged version must pass before it may be swapped in: the ports the app
// implements (omp adapters live in loa-qt) and the pure checks over their answers.
//
// Ports are blocking calls; `run_gates` queries them concurrently on threads of its own.

#include <chrono>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <map>
#include <memory>
#include <optional>
#include <set>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/runtime/json.hpp"
#include "loa/runtime/semver.hpp"
#include "loa/settings/updates.hpp"
#include "loa/updater/error.hpp"

namespace loa::updater {

using Json = loa::runtime::json::Value;
using loa::settings::CheckResult;
using loa::settings::GateCheck;
using loa::settings::GateReport;

// One tool as a version advertises it, with the JSON Schema of its input.
struct ToolSchema {
    std::string name;
    Json input_schema;
};

// Tools the app calls, each with the dotted argument fields it fills in.
using UsedTools = std::map<std::string, std::set<std::string>>;

// Config keys a version accepts; keys below an `open` record are accepted too.
struct ConfigSchema {
    std::set<std::string> keys;
    std::set<std::string> open;
    // Aliases the version declares: old key -> new key.
    std::map<std::string, std::string> renamed;

    [[nodiscard]] bool accepts(std::string_view key) const;
    friend bool operator==(const ConfigSchema&, const ConfigSchema&) = default;
};

// One contract test whose output no longer matches its golden file.
struct ContractFailure {
    std::string test;
    std::string diff;
    friend bool operator==(const ContractFailure&, const ContractFailure&) = default;
};

// Changelog entries the reviewer flagged.
struct ChangelogReview {
    std::vector<std::string> breaking;
    std::vector<std::string> removed;
    friend bool operator==(const ChangelogReview&, const ChangelogReview&) = default;
};

// What the app depends on in omp: used tools and fields, written config keys, migrations, RPC range.
struct OmpGates {
    UsedTools used_tools;
    std::set<std::string> written_keys;
    // Session format migrations the adapter has, as `(from, to)` steps.
    std::set<std::pair<std::uint32_t, std::uint32_t>> migrations;
    // Inclusive range.
    std::pair<std::uint32_t, std::uint32_t> rpc_supported{1, 2};

    // The app's real dependencies (Rust `OmpGates::default()`).
    [[nodiscard]] static OmpGates defaults();
    friend bool operator==(const OmpGates&, const OmpGates&) = default;
};

template <typename T>
using PortResult = std::expected<T, std::string>;

// Lists the tools an installed version offers.
class ToolCatalog {
  public:
    virtual ~ToolCatalog() = default;
    virtual PortResult<std::vector<ToolSchema>> tools(const std::filesystem::path& executable) = 0;
};

// Asks an installed version for its session format and config schema.
class FormatProbe {
  public:
    virtual ~FormatProbe() = default;
    virtual PortResult<std::uint32_t> session_version(const std::filesystem::path& executable) = 0;
    virtual PortResult<ConfigSchema> config_keys(const std::filesystem::path& executable) = 0;
};

// Asks an installed version which RPC protocol versions it offers.
class RpcProbe {
  public:
    virtual ~RpcProbe() = default;
    virtual PortResult<std::vector<std::uint32_t>> protocol_versions(const std::filesystem::path& executable) = 0;
};

// Runs the adapter contract tests against an installed version.
class ContractRunner {
  public:
    virtual ~ContractRunner() = default;
    virtual PortResult<std::vector<ContractFailure>> run(const std::filesystem::path& executable) = 0;
};

// Reviews changelog text for entries that break the app.
class ChangelogSummarizer {
  public:
    virtual ~ChangelogSummarizer() = default;
    virtual PortResult<ChangelogReview> review(const std::string& changelog) = 0;
};

// Every probe port of a fully gated product. Calls may come from several threads at once.
class Probe : public ToolCatalog, public FormatProbe, public RpcProbe, public ContractRunner {};

// Builds the gate adapters of one gate run; the app plugs in the omp-backed ones.
class GatePorts {
  public:
    virtual ~GatePorts() = default;
    virtual std::unique_ptr<Probe> probe(const OmpGates& gates, std::chrono::milliseconds timeout) = 0;
    // A reviewer running `model` on the `reviewer` executable.
    virtual std::unique_ptr<ChangelogSummarizer> reviewer(const std::filesystem::path& reviewer, const std::string& model,
                                                          std::chrono::milliseconds timeout) = 0;
};

// Everything the gate checks need; null skips the checks that don't apply.
struct GateInputs {
    std::filesystem::path old_executable;
    std::filesystem::path new_executable;
    Probe* probe = nullptr;
    const OmpGates* omp = nullptr;
    // The reviewer and the release notes between the two versions.
    ChangelogSummarizer* reviewer = nullptr;
    std::string changelog;
};

// Runs the applicable checks with their ports queried concurrently; a port error fails only its check.
[[nodiscard]] GateReport run_gates(std::string_view product, const Version& from, const Version& to, const GateInputs& inputs);

// Compares two versions' tools. Harmful when a used tool or field is gone or changed type, or a field
// of a used tool became required. Additions are `+` lines; a used tool neither offers is a `~` line.
[[nodiscard]] CheckResult check_tool_schemas(const std::vector<ToolSchema>& old_tools, const std::vector<ToolSchema>& new_tools,
                                             const UsedTools& used);
// Harmful when the session format changes and the migrations offer no chain from `old` to `new`.
[[nodiscard]] CheckResult check_session_format(std::uint32_t old_version, std::uint32_t new_version,
                                               const std::set<std::pair<std::uint32_t, std::uint32_t>>& migrations);
// Harmful when a key the app writes is absent from the new schema.
[[nodiscard]] CheckResult check_config_schema(const std::set<std::string>& written_keys, const ConfigSchema& schema);
// Harmful when none of the offered versions is in the inclusive `supported` range.
[[nodiscard]] CheckResult check_rpc_protocol(const std::vector<std::uint32_t>& offered, std::pair<std::uint32_t, std::uint32_t> supported);
// Harmful when any contract test failed; details list each test and its diff.
[[nodiscard]] CheckResult check_contract(const std::vector<ContractFailure>& failures);
// Harmful when the review flagged any entry.
[[nodiscard]] CheckResult check_changelog(const ChangelogReview& review);

// Instructions of a changelog review; the changelog follows them.
extern const std::string_view REVIEW_PROMPT;

// Parses a reviewer reply: exactly one JSON object, optionally in a single ```json block.
[[nodiscard]] std::expected<ChangelogReview, std::string> parse_review(std::string_view text);
// Joins release notes into one changelog, oldest first, each under `## <version>`.
[[nodiscard]] std::string changelog_text(const std::vector<std::pair<Version, std::string>>& notes);

// serde JSON forms.
[[nodiscard]] Json to_json(const ToolSchema& v);
[[nodiscard]] Json to_json(const ConfigSchema& v);
[[nodiscard]] Json to_json(const ContractFailure& v);
[[nodiscard]] Json to_json(const ChangelogReview& v);
[[nodiscard]] Json to_json(const OmpGates& v);
[[nodiscard]] std::expected<OmpGates, std::string> omp_gates_from_json(const Json& v);

// serde_json `Value` equality: objects compare as maps, regardless of key order.
[[nodiscard]] bool json_equal(const Json& a, const Json& b);

} // namespace loa::updater
