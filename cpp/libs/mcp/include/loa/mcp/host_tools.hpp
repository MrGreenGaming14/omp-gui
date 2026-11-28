#pragma once

// Running servers' tools as host tools, for harnesses that take the app's tools that way
// (omp). Calls go through the `Manager`, so the filter's project toggles and peer grants
// apply to every call.

#include <map>
#include <memory>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/contracts/output.hpp"
#include "loa/mcp/manager.hpp"
#include "loa/mcp/tool_filter.hpp"

namespace loa::mcp::host_tools {

// `server__tool`, with characters model APIs refuse replaced by `_`. Not `mcp__…`: omp may
// mount tools named like its own MCP tools as `xd://` devices.
[[nodiscard]] std::string host_tool_name(std::string_view server, std::string_view tool);

// What a model can read of a tool result; structured content stands in when there is no other content.
[[nodiscard]] contracts::ToolOutput to_output(const model::CallToolResult& result);
// Text of `output` with images as placeholders.
[[nodiscard]] std::string text(const contracts::ToolOutput& output);

// A snapshot of the allowed tools of every running server, each routed back to its server.
// The blocking MCP call runs on its own thread; the coroutine resumes on its executor.
class McpHostTools final : public contracts::HostTools {
  public:
    McpHostTools(Manager manager, std::shared_ptr<const ToolFilter> filter);

    [[nodiscard]] std::span<const contracts::HostToolSpec> specs() const noexcept { return specs_; }
    [[nodiscard]] bool empty() const noexcept { return specs_.empty(); }

    // The call, run on the calling thread.
    [[nodiscard]] std::expected<contracts::ToolOutput, std::string> run(std::string_view tool, std::string_view args) const;

    [[nodiscard]] contracts::Awaitable<std::expected<std::string, std::string>> call(std::string session_id, std::string tool,
                                                                                    std::string args) const override;
    [[nodiscard]] contracts::Awaitable<std::expected<std::vector<contracts::OutputBlock>, std::string>> call_content(
        std::string session_id, std::string tool, std::string args) const override;

  private:
    Manager manager_;
    std::shared_ptr<const ToolFilter> filter_;
    std::vector<contracts::HostToolSpec> specs_;
    std::map<std::string, std::pair<std::string, std::string>, std::less<>> routes_;
};

} // namespace loa::mcp::host_tools
