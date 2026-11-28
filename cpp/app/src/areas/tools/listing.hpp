#pragma once

// What the Tools panel lists, built from the services: MCP servers with their tools, import
// candidates, and the tools each peer's grants allow (port of
// `loa-app/src/areas/tools/listing.rs`).

#include <cstdint>
#include <span>
#include <string>
#include <string_view>
#include <tuple>
#include <vector>

#include "loa/mcp/import.hpp"
#include "loa/mcp/manager.hpp"
#include "loa/trust/store.hpp"
#include "loa/view/panel.hpp"

namespace loa::app::areas::tools::listing {

// Prefix of the tools surface servers expose.
inline constexpr std::string_view SURFACE_TOOL_PREFIX = "surface_";
// Characters of a tool description the panel shows.
inline constexpr std::size_t DESCRIPTION_CHARS = 160;

// The first line of a tool's description, shortened for a list row.
[[nodiscard]] std::string short_description(std::string_view text);
// A server the manager runs, as a panel row. Servers whose tools are `surface_*` tools are
// marked as surface servers.
[[nodiscard]] view::panel::McpServer server(const mcp::ServerView& view);
// An app hosted server row.
[[nodiscard]] view::panel::McpServer app_server(std::string_view name, contracts::McpTransport transport,
                                                view::panel::ServerStatus status, std::vector<view::panel::ToolInfo> tools);
[[nodiscard]] view::panel::ImportSource import_source(mcp::import::App app);
// The usable entries of a scan as import candidates.
[[nodiscard]] std::vector<view::panel::ImportCandidate> candidates(std::span<const mcp::import::Found> found);
// The scanned entries the user picked.
[[nodiscard]] std::vector<mcp::import::Found> picked(std::span<const mcp::import::Found> found,
                                                     std::span<const view::panel::ImportCandidate> chosen);
// What happened to each imported entry, for the import list.
[[nodiscard]] std::vector<std::tuple<view::panel::ImportSource, std::string, view::panel::ImportOutcome>> outcomes(
    const mcp::import::ImportReport& report);
// Per peer, the tools its live grants let it have the agent use, as the grant checks decide them:
// - every tool needs drive on a session (or a turn, project, future sessions)
// - `remote` tools also need an SSH grant on a known host: exec for `remote_bash`, read for the others
// - `surface_*` tools need a grant on a surface of their server: view for `surface_screenshot`, drive otherwise
[[nodiscard]] std::vector<view::panel::PeerTools> peer_tools(std::span<const trust::IssuedRecord> issued,
                                                             std::span<const view::panel::McpServer> servers,
                                                             std::span<const std::string> hosts, std::int64_t now_ms);

} // namespace loa::app::areas::tools::listing
