#pragma once

// The app's central config as the server store, and its per project tool toggles.

#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/mcp/registry.hpp"
#include "loa/mcp/spec.hpp"
#include "loa/mcp/tool_filter.hpp"
#include "loa/settings/app/store.hpp"

namespace loa::mcp::central {

// A server of the central config; one that doesn't validate is still returned and the manager refuses to run it.
[[nodiscard]] ServerSpec from_config(std::string_view name, const settings::app::McpServer& server);
// The contract's transport for a spec; a stdio server's working directory is dropped.
[[nodiscard]] contracts::McpTransport transport(const ServerSpec& spec);
[[nodiscard]] settings::app::McpServer to_config(const ServerSpec& spec);
// The server as a harness is told about it in its `SessionConfig`.
[[nodiscard]] contracts::McpServerSpec session_spec(const ServerSpec& spec);
// A server typed in the Tools panel's add form.
[[nodiscard]] ServerSpec from_session_spec(const contracts::McpServerSpec& spec);

// `ServerStore` over the `mcp_servers` table of the central config; a save replaces only that
// table, so other keys written in between survive.
class ConfigServers final : public ServerStore {
  public:
    explicit ConfigServers(settings::app::ConfigStore store) : store_(std::move(store)) {}
    [[nodiscard]] std::expected<std::vector<ServerSpec>, StoreError> load() const override;
    [[nodiscard]] std::expected<void, StoreError> save(const std::vector<ServerSpec>& servers) const override;

  private:
    settings::app::ConfigStore store_;
};

// The MCP tool toggles of one project.
class ProjectFilter final : public ToolFilter {
  public:
    ProjectFilter() = default;
    explicit ProjectFilter(settings::app::ProjectSettings settings) : settings_(std::move(settings)) {}
    [[nodiscard]] bool allows(std::string_view server, std::string_view tool) const override;

  private:
    settings::app::ProjectSettings settings_;
};

// What a harness that connects to MCP servers itself calls an MCP tool (Claude Code's `mcp__server__tool`).
[[nodiscard]] std::string harness_tool_name(std::string_view server, std::string_view tool);

// A session's `disabled_tools` for a project: built in tools as they are, MCP tools as `harness_tool_name`s.
[[nodiscard]] std::vector<std::string> session_disabled_tools(const settings::app::ProjectSettings& settings);

} // namespace loa::mcp::central
