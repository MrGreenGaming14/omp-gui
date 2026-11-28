#pragma once

// The app's surface service: `SurfaceHub` over the `SurfaceProvider`s (browser,
// docs, vm, MCP declared), the `surface_*` agent tools (`SurfaceTools`) as host
// tools or behind the loopback `McpEndpoint`, and the `<surface>` session
// elements (`hub::dom`). Port of `loa-surfaces/src/hub/mod.rs`.

#include "loa/surfaces/hub/dom.hpp"
#include "loa/surfaces/hub/endpoint.hpp"
#include "loa/surfaces/hub/error.hpp"
#include "loa/surfaces/hub/mcp.hpp"
#include "loa/surfaces/hub/mcp_surface.hpp"
#include "loa/surfaces/hub/providers.hpp"
#include "loa/surfaces/hub/service.hpp"
#include "loa/surfaces/hub/tools.hpp"
