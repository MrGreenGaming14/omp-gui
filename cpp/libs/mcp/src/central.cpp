#include "loa/mcp/central.hpp"

namespace loa::mcp::central {

namespace app = loa::settings::app;

ServerSpec from_config(std::string_view name, const app::McpServer& server) {
    Transport transport;
    if (const auto* stdio = std::get_if<contracts::McpStdio>(&server.transport)) {
        transport = StdioSpec{stdio->command, stdio->args, stdio->env, std::nullopt};
    } else {
        const auto& http = std::get<contracts::McpHttp>(server.transport);
        transport = HttpSpec{http.url, http.headers};
    }
    return ServerSpec{std::string(name), server.enabled, std::move(transport)};
}

contracts::McpTransport transport(const ServerSpec& spec) {
    if (const auto* stdio = std::get_if<StdioSpec>(&spec.transport)) return contracts::McpStdio{stdio->command, stdio->args, stdio->env};
    const auto& http = std::get<HttpSpec>(spec.transport);
    return contracts::McpHttp{http.url, http.headers};
}

app::McpServer to_config(const ServerSpec& spec) { return app::McpServer{transport(spec), spec.enabled}; }

contracts::McpServerSpec session_spec(const ServerSpec& spec) { return contracts::McpServerSpec{spec.name, transport(spec)}; }

ServerSpec from_session_spec(const contracts::McpServerSpec& spec) {
    return from_config(spec.name, app::McpServer{spec.transport, true});
}

std::expected<std::vector<ServerSpec>, StoreError> ConfigServers::load() const {
    auto config = store_.load();
    if (!config) return std::unexpected(config.error().to_string());
    std::vector<ServerSpec> out;
    for (const auto& [name, server] : config->mcp_servers) out.push_back(from_config(name, server));
    return out;
}

std::expected<void, StoreError> ConfigServers::save(const std::vector<ServerSpec>& servers) const {
    std::map<std::string, app::McpServer> table;
    for (const auto& spec : servers) table.insert_or_assign(spec.name, to_config(spec));
    auto edited = store_.edit([&](app::Config& config) { config.mcp_servers = std::move(table); });
    if (!edited) return std::unexpected(edited.error().to_string());
    return {};
}

bool ProjectFilter::allows(std::string_view server, std::string_view tool) const { return settings_.mcp_tool_enabled(server, tool); }

std::string harness_tool_name(std::string_view server, std::string_view tool) {
    return "mcp__" + std::string(server) + "__" + std::string(tool);
}

std::vector<std::string> session_disabled_tools(const app::ProjectSettings& settings) {
    std::vector<std::string> out(settings.disabled_tools.begin(), settings.disabled_tools.end());
    for (const auto& [server, tools] : settings.disabled_mcp_tools)
        for (const auto& tool : tools) out.push_back(harness_tool_name(server, tool));
    return out;
}

} // namespace loa::mcp::central
