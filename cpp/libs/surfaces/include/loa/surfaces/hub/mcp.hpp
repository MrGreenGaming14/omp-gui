#pragma once

// Surfaces declared as MCP resources: `uri` is the `surface://` uri, `name` the
// label, `_meta["omp/surface"]` the pixel size and mode; documents set `mimeType`.
// Port of `loa-surfaces/src/hub/mcp.rs`.

#include <expected>
#include <optional>
#include <string>
#include <string_view>

#include "loa/contracts/surface.hpp"
#include "loa/runtime/json.hpp"

namespace loa::surfaces::hub {

namespace cs = loa::contracts::surface;

// `_meta` key of the surface fields on a resource.
inline constexpr std::string_view META_KEY = "omp/surface";

// The MCP resource that declares `info`.
[[nodiscard]] loa::runtime::json::Value resource(const cs::SurfaceInfo& info);

// The surface a resource declares; none for resources that are not `surface://` uris. The error
// is the Rust `McpError` display text.
[[nodiscard]] std::expected<std::optional<cs::SurfaceInfo>, std::string> parse_resource(const loa::runtime::json::Value& resource);

} // namespace loa::surfaces::hub
