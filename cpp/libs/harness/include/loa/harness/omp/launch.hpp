#pragma once

// Process arguments and state files for an app-managed omp session.

#include <chrono>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <utility>

#include "loa/contracts/harness.hpp"
#include "loa/harness/omp/rpc/client.hpp"

namespace loa::harness::omp {

enum class MemoryMode {
    // Deliver app digests via --append-system-prompt and disable omp memory.
    AppDigests,
    // Keep omp's configured memory backend; ignore the app's project context.
    OmpMemory,
};

struct OmpAdapterConfig {
    std::string node;
    std::filesystem::path executable = "omp";
    MemoryMode memory = MemoryMode::AppDigests;
    std::filesystem::path state_dir;
    std::optional<std::string> approval_mode;
    std::optional<std::filesystem::path> session_dir;
    std::chrono::milliseconds ready_timeout = std::chrono::seconds(60);
    std::chrono::milliseconds shutdown_grace = std::chrono::seconds(10);

    OmpAdapterConfig(std::string node, std::filesystem::path state_dir)
        : node(std::move(node)), state_dir(std::move(state_dir)) {}
};

// omp's default-on built-in tools. Host tools are independent of this list.
inline constexpr std::string_view DEFAULT_TOOLS[] = {
    "read", "edit", "write", "bash", "grep", "glob", "find", "eval", "task", "wait", "todo", "ask",
    "web_search", "lsp", "ast_edit", "debug",
};

struct Target {
    enum class Kind { New, Resume } kind = Kind::New;
    std::string id;

    [[nodiscard]] static Target resume(std::string id) { return {Kind::Resume, std::move(id)}; }
};

// Builds the command options and writes the referenced memory and prompt files.
[[nodiscard]] loa::contracts::Result<rpc::SpawnOptions> spawn_options(
    const OmpAdapterConfig& adapter, const loa::contracts::SessionConfig& session, const Target& target);

// Removes unavailable built-ins named by omp's startup error. Returns false when
// stderr is unrelated or the --tools argument would not change.
[[nodiscard]] bool drop_unavailable_tools(rpc::SpawnOptions& opts, std::string_view stderr);

// Splits on the first slash; both provider and model must be nonempty.
[[nodiscard]] loa::contracts::Result<std::pair<std::string_view, std::string_view>> split_selector(std::string_view selector);

} // namespace loa::harness::omp
