#pragma once

// The Surfaces panel tab: browser tabs, documents, VMs and surfaces MCP servers declare, drawn
// by the generic surface renderer, and the `surface_*` tools sessions drive them with (port of
// `loa-app/src/areas/surfaces/{mod,video}.rs`).

// First: `SurfaceHub` declares a member named `emit`, which Qt headers define away.
#include "loa/surfaces/hub/endpoint.hpp"
#include "loa/surfaces/hub/service.hpp"
#include "loa/surfaces/hub/tools.hpp"
#include "loa/surfaces/video/live.hpp"

#include <functional>
#include <map>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/app/area.hpp"
#include "loa/app/routes.hpp"
#include "loa/ui/surface/panel.hpp"

namespace loa::ui::surface {
class Runner;
}

namespace loa::app::areas::surfaces {

namespace hub = loa::surfaces::hub;
namespace panel = loa::ui::surface::panel;
using loa::surfaces::CaptureMode;
using loa::surfaces::video::LiveVideo;
using SurfaceUri = loa::contracts::surface::SurfaceUri;

// Id of the Surfaces panel tab.
inline constexpr std::string_view TAB = "surfaces";

// The running loopback MCP endpoint, shared by the sessions that use it.
using Endpoint = std::shared_ptr<const hub::McpEndpoint>;

namespace msg {
struct Panel {
    panel::Message message;
};
// Opening a surface from the launcher finished.
struct Opened {
    std::expected<hub::Opened, std::string> result;
};
// Closing a surface finished.
struct Closed {
    std::expected<void, std::string> result;
};
// Something changed in the hub.
struct Hub {
    hub::HubEvent event;
};
// The hub's surface list, for the launcher.
struct Listed {
    std::vector<hub::Listed> listed;
};
// The MCP servers changed: read the surfaces they declare again.
struct Mcp {};
// Reading declared surfaces finished; the servers whose listing failed.
struct Declared {
    std::vector<std::pair<std::string, std::string>> failed;
};
// The loopback MCP endpoint started.
struct EndpointStarted {
    std::expected<Endpoint, std::string> result;
};
// The machine's video encoders were tried.
struct Encoders {
    std::shared_ptr<const LiveVideo> encoders;
};
// Whether the running browser keeps the other capture until its tabs close.
struct CapturePending {
    bool pending = false;
};
// Work a tab's runner hands to the GUI thread.
struct Run {
    std::shared_ptr<std::function<void()>> run;
};
} // namespace msg

using Message = std::variant<msg::Panel, msg::Opened, msg::Closed, msg::Hub, msg::Listed, msg::Mcp, msg::Declared,
                             msg::EndpointStarted, msg::Encoders, msg::CapturePending, msg::Run>;

// What the launcher says about live video: the encoder this app's renderer gets (it decodes
// H.264) and the hardware encoders that work (`video.rs`).
[[nodiscard]] loa::view::surface::VideoStatus video_status(bool enabled, const LiveVideo& encoders);

// The surfaces shown in tabs and the runners serving their tabs, shared with the jobs that
// start runners.
struct Tabs {
    std::map<SurfaceUri, std::shared_ptr<loa::contracts::surface::Surface>> surfaces;
    std::map<SurfaceUri, std::unique_ptr<loa::ui::surface::Runner>> runners;
    Tabs();
    ~Tabs();
};

class State final : public TypedArea<State> {
  public:
    static constexpr std::string_view ID = area_id::SURFACES;
    using Message = surfaces::Message;

    [[nodiscard]] static std::pair<std::unique_ptr<State>, Task> make(Ctx& ctx);
    // The area over `hub`; starts the MCP endpoint and reads the surfaces the MCP servers declare.
    [[nodiscard]] static std::pair<std::unique_ptr<State>, Task> with_hub(std::shared_ptr<hub::SurfaceHub> hub, Ctx& ctx);
    [[nodiscard]] static std::span<const AreaCommand<State>> COMMANDS();

    State(std::shared_ptr<hub::SurfaceHub> hub, std::shared_ptr<hub::SurfaceTools> tools,
          std::shared_ptr<mcp::Manager> mcp, loa::ui::surface::Panel panel);
    ~State() override;

    Task update_typed(Message message, Ctx& ctx);
    [[nodiscard]] std::vector<PanelTab> panel_tabs() const override;
    [[nodiscard]] QWidget* view_tab(std::string_view id, const Ctx& ctx, const Emit& send) const override;
    void contribute(contracts::SessionConfig& config, const Target& target, const Ctx& ctx) const override;
    [[nodiscard]] std::optional<HostToolSet> host_tools(const contracts::SessionConfig& config, const Target& target,
                                                        const Ctx& ctx) const override;
    Task on_session_event(const SessionEvent& event, Ctx& ctx) override;
    [[nodiscard]] Subscriptions subscription() const override;

    [[nodiscard]] const std::shared_ptr<hub::SurfaceHub>& hub() const noexcept { return hub_; }
    [[nodiscard]] const loa::ui::surface::Panel& panel() const noexcept { return panel_; }
    [[nodiscard]] const Endpoint& endpoint() const noexcept { return endpoint_; }

    // Opens the launcher on the Surfaces tab.
    Task launcher(Ctx& ctx);

  private:
    // The hub's surface list and whether the browser's capture is pending.
    [[nodiscard]] Task list() const;
    [[nodiscard]] Task refresh_declared() const;
    Task set_capture(loa::view::surface::Capture capture, Ctx& ctx);
    Task set_video(bool enabled, Ctx& ctx);
    Task panel_event(loa::view::surface::Intent intent, Ctx& ctx);
    // Runs the surface work of a panel update on the tabs' runners.
    [[nodiscard]] Task run_panel(panel::Task task) const;
    Task show(const hub::Opened& opened, Ctx& ctx);
    void record(const hub::Opened& opened, Ctx& ctx);
    void record_closed(const SurfaceUri& uri, Ctx& ctx);
    void detach(const SurfaceUri& uri);

    std::shared_ptr<hub::SurfaceHub> hub_;
    std::shared_ptr<hub::SurfaceTools> tools_;
    Endpoint endpoint_;
    std::shared_ptr<mcp::Manager> mcp_;
    loa::ui::surface::Panel panel_;
    std::shared_ptr<Tabs> tabs_;
    // The `<surface>` elements recorded for each surface: session and node.
    std::map<SurfaceUri, std::vector<std::pair<SessionId, core::NodeId>>> recorded_;
    // Surfaces opened while no session was shown; recorded in the next one shown.
    std::vector<SurfaceUri> unrecorded_;
};

} // namespace loa::app::areas::surfaces
