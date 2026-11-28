#pragma once

// omp's own tools served to other harnesses over MCP. omp can't run a tool directly, so the
// bridge runs `omp --mode rpc` against a loopback model it serves itself (`provider.hpp`):
// each call becomes a prompt the scripted model answers with exactly that tool call, and the
// `tool_execution_end` result goes back to the MCP client. No model is ever contacted.

#include <array>
#include <expected>
#include <filesystem>
#include <memory>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/contracts/output.hpp"
#include "loa/contracts/tools.hpp"
#include "loa/mcp/error.hpp"
#include "loa/runtime/json.hpp"

namespace loa::mcp::bridge {

// omp tools offered by default: hashline reads and edits, AST search and edits, LSP and DAP.
inline constexpr std::array<std::string_view, 6> DEFAULT_TOOLS = {"read", "edit", "ast_grep", "ast_edit", "lsp", "debug"};
// omp tools that change files or run things.
inline constexpr std::array<std::string_view, 5> MUTATING_TOOLS = {"edit", "ast_edit", "debug", "write", "bash"};
// MCP server name harnesses see; their tool names become `mcp__omp__read`.
inline constexpr std::string_view SERVER_NAME = "omp";
inline constexpr std::string_view INSTRUCTIONS =
    "omp's own tools. `edit` takes hashline patches: read a file with this server's `read` first and copy the `[path#TAG]` "
    "header it returns.";
inline constexpr std::string_view PROVIDER_ID = "loa-omp-bridge";
inline constexpr std::string_view MODEL_ID = "scripted";
inline constexpr std::string_view TOKEN_ENV = "LOA_OMP_BRIDGE_TOKEN";
// Set by omp for processes its tools spawn; the bridge's omp is never part of that session.
inline constexpr std::array<std::string_view, 4> INHERITED_SESSION_ENV = {"PI_SESSION_FILE", "PI_SESSION_ID", "PI_TOOL_CALL_ID",
                                                                          "PI_TERMINAL_OWNER_PID"};
// Tool output replayed in omp's transcript before the bridge starts a fresh omp session.
inline constexpr std::size_t HISTORY_BUDGET_BYTES = std::size_t{8} << 20;
inline constexpr std::string_view USAGE =
    "usage: loa omp-bridge --state-dir DIR [--cwd DIR] [--omp PATH] [--tools a,b,...] [--config FILE]...";

// How to run omp for the bridge.
struct OmpBridgeConfig {
    std::filesystem::path program = "omp";
    // Workspace the tools act on.
    std::filesystem::path cwd;
    // Owned by the bridge; omp's agent directory lives inside it, never `~/.omp/agent`.
    std::filesystem::path state_dir;
    std::vector<std::string> tools;
    // Extra `--config` overlays, such as the user's omp config for LSP settings.
    std::vector<std::filesystem::path> config_overlays;
    std::vector<std::pair<std::string, std::string>> env;

    // Runs `omp` from `PATH` on `cwd` with `DEFAULT_TOOLS`.
    [[nodiscard]] static OmpBridgeConfig make(std::filesystem::path cwd, std::filesystem::path state_dir);

    // Reads `--state-dir DIR [--cwd DIR] [--omp PATH] [--tools a,b] [--config FILE]...`.
    [[nodiscard]] static std::expected<OmpBridgeConfig, std::string> parse(std::span<const std::string> args);

    // The arguments `parse` reads back into this config.
    [[nodiscard]] std::vector<std::string> args() const;

    // The stdio MCP server running this bridge as `program` with `prefix` arguments (the app
    // passes its own executable and `{"omp-bridge"}`).
    [[nodiscard]] contracts::McpServerSpec session_spec(const std::filesystem::path& program,
                                                        std::span<const std::string_view> prefix) const;
};

// `<state_dir>/agent/models.yml`: the scripted provider at `base_url` (JSON, which is YAML).
[[nodiscard]] std::string models_config(std::string_view base_url);
// `<state_dir>/bridge-config.yml`: the overlay enabling AST tools and disabling xdev.
[[nodiscard]] std::string bridge_config();

// Picks the wanted tools out of `get_state`'s `dumpTools`, keeping omp's schemas, in the given order.
[[nodiscard]] Result<std::vector<contracts::HostToolSpec>> select_tools(const runtime::json::Array& dumped,
                                                                        std::span<const std::string> wanted);
// The result of a `tool_execution_end` frame.
[[nodiscard]] contracts::ToolOutput tool_output(const runtime::json::Value* result, bool is_error);

// Runs omp's tools in an omp RPC process, one call at a time.
class OmpExecutor : public std::enable_shared_from_this<OmpExecutor> {
  public:
    struct Impl;

    // Starts the loopback provider and omp, reading each tool's schema from omp.
    [[nodiscard]] static contracts::Awaitable<Result<std::shared_ptr<OmpExecutor>>> start(OmpBridgeConfig config);
    ~OmpExecutor();

    [[nodiscard]] const std::vector<contracts::HostToolSpec>& tools() const noexcept { return tools_; }
    // The executor's tools as a `ToolSet`.
    [[nodiscard]] contracts::ToolSet tool_set();

    // Runs `name`. Cancelling the coroutine (terminal cancellation) aborts omp's turn.
    [[nodiscard]] contracts::Awaitable<contracts::ToolResult> call(std::string name, runtime::json::Object arguments);

  private:
    OmpExecutor(std::unique_ptr<Impl> impl, std::vector<contracts::HostToolSpec> tools);
    std::unique_ptr<Impl> impl_;
    std::vector<contracts::HostToolSpec> tools_;
};

// Every tool omp enables by default, with its schema, read from an omp run the bridge's way
// (no model contacted). `config.tools` is ignored.
[[nodiscard]] contracts::Awaitable<Result<std::vector<contracts::HostToolSpec>>> list_omp_tools(OmpBridgeConfig config);

// Serves the bridge on stdin and stdout until the client leaves (`loa omp-bridge`); the
// process exit code.
[[nodiscard]] int run_cli(std::span<const std::string> args);

} // namespace loa::mcp::bridge
