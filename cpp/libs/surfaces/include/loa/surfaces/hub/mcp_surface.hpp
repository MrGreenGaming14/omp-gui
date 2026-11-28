#pragma once

// A surface a third party MCP server declares, viewed through its own
// `surface_*` tools. Port of `loa-surfaces/src/hub/mcp_surface.rs`.
//
// The frame and input stream (`omp/surface/1`) is not reachable through MCP
// alone, so the picture comes from `surface_screenshot` (taken when a viewer
// subscribes, after each input and twice a second while watched) and input
// goes out as `surface_click`, `surface_type` and `surface_scroll` calls.
// Mouse moves, drags and keys other than Enter have no tool and are not
// sent. Document surfaces are read once with `resources/read`.

#include <memory>
#include <string>

#include "loa/contracts/async.hpp"
#include "loa/contracts/surface.hpp"
#include "loa/mcp/manager.hpp"

namespace loa::surfaces::hub {

namespace cs = loa::contracts::surface;

class McpSurface final : public cs::Surface {
  public:
    // `info` as the server declared it; `server` is its name in `manager`. Pixel surfaces poll
    // screenshots on `executor` while watched.
    McpSurface(mcp::Manager manager, std::string server, cs::SurfaceInfo info, const asio::any_io_executor& executor);
    ~McpSurface() override;
    McpSurface(const McpSurface&) = delete;
    McpSurface& operator=(const McpSurface&) = delete;

    [[nodiscard]] cs::SurfaceInfo info() const override;
    [[nodiscard]] cs::Result<std::shared_ptr<cs::FrameReceiver>> frames() override;
    [[nodiscard]] loa::contracts::Awaitable<cs::Result<void>> request_keyframe() override;
    [[nodiscard]] loa::contracts::Awaitable<cs::Result<void>> input(cs::InputEvent event) override;
    [[nodiscard]] loa::contracts::Awaitable<cs::Result<cs::Document>> document() override;
    [[nodiscard]] loa::contracts::Awaitable<cs::Result<cs::Bytes>> screenshot() override;

    struct Shared;

  private:
    // The surface's info, or a mode error unless it is in `mode`.
    [[nodiscard]] cs::Result<cs::SurfaceInfo> in_mode(cs::SurfaceMode mode) const;

    std::shared_ptr<Shared> shared_;
};

} // namespace loa::surfaces::hub
