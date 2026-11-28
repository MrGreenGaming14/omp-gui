#include "loa/settings/app/schema.hpp"

#include <algorithm>

namespace loa::settings::app {

std::string UnknownKey::dotted() const {
    std::string s;
    for (const auto& seg : path) {
        if (!s.empty()) s += '.';
        s += seg;
    }
    return s;
}

std::string_view to_string(BrowserCapture c) noexcept {
    return c == BrowserCapture::VirtualDisplay ? "virtual-display" : "screencast";
}

std::optional<std::filesystem::path> HarnessSettings::executable(std::string_view harness) const {
    auto it = executables.find(std::string(harness));
    if (it == executables.end()) return std::nullopt;
    return it->second;
}

std::string Relay::to_string() const {
    if (std::holds_alternative<Default>(value)) return "default";
    if (std::holds_alternative<Off>(value)) return "off";
    return std::get<Url>(value).url;
}

Relay Relay::from_string(std::string text) {
    if (text == "default") return Relay{Default{}};
    if (text == "off") return Relay{Off{}};
    return Relay{Url{std::move(text)}};
}

const DecisionDefault* UnattendedSettings::for_kind(loa::core::DecisionKind kind) const noexcept {
    using K = loa::core::DecisionKind;
    const std::optional<DecisionDefault>* d = nullptr;
    switch (kind) {
    case K::Approval: d = &approval; break;
    case K::Question: d = &question; break;
    case K::Plan: d = &plan; break;
    case K::Form: d = &form; break;
    }
    return d && *d ? &**d : nullptr;
}

loa::contracts::McpServerSpec McpServer::spec(std::string_view name) const {
    return {std::string(name), transport};
}

bool ProjectSettings::mcp_tool_enabled(std::string_view server, std::string_view tool) const {
    auto it = disabled_mcp_tools.find(std::string(server));
    return it == disabled_mcp_tools.end() || !it->second.contains(std::string(tool));
}

ProjectSettings Config::project(std::string_view id) const {
    auto it = projects.find(std::string(id));
    return it == projects.end() ? ProjectSettings{} : it->second;
}

void Config::apply_to_session(std::string_view harness_name, loa::contracts::SessionConfig& session) const {
    for (const auto& [name, server] : mcp_servers)
        if (server.enabled) session.mcp_servers.push_back(server.spec(name));
    if (session.project) {
        if (auto it = projects.find(*session.project); it != projects.end())
            session.disabled_tools.insert(session.disabled_tools.end(), it->second.disabled_tools.begin(),
                                          it->second.disabled_tools.end());
    }
    if (!session.executable) session.executable = harness.executable(harness_name);
}

} // namespace loa::settings::app
