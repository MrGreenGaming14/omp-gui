#pragma once

// Tools: the Tools panel tab (built in tools, MCP servers, Remote hosts, what each peer may use,
// per project toggles), the tools every session gets (registered MCP servers, the app-hosted
// `remote` server, the omp tools bridge for other harnesses) and the Remote shell tab (port of
// `loa-app/src/areas/tools`).

#include <cstdint>
#include <expected>
#include <filesystem>
#include <map>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "areas/tools/remote.hpp"
#include "loa/app/area.hpp"
#include "loa/app/routes.hpp"
#include "loa/mcp/import.hpp"
#include "loa/mcp/registry.hpp"
#include "loa/ui/panel/tools.hpp"
#include "loa/ui/terminal/state.hpp"

namespace loa::app::areas::tools {

namespace panel_tools = loa::ui::panel::tools;
namespace terminal = loa::ui::terminal;
namespace vp = loa::view::panel;

// Id of the Tools panel tab.
inline constexpr std::string_view TAB = "tools";
// Id of the Remote shell tab: terminals on Remote hosts.
inline constexpr std::string_view SHELL_TAB = "remote-shell";
// The harness whose built in tools the app can list, and which serves them to the other
// harnesses through the bridge.
inline constexpr std::string_view OMP = "omp";

// Whether `decision` is a card the `remote` server raised (a host key or approval question),
// answered by this area rather than the harness.
[[nodiscard]] bool is_remote_decision(const core::Dom& dom, core::NodeId decision);

using Servers = mcp::Registry;

namespace msg {
struct Panel {
    panel_tools::Message message;
};
// The registered MCP servers were loaded and started.
struct ServersOpened {
    std::expected<std::shared_ptr<Servers>, std::string> result;
};
// The `remote` server's loopback HTTP endpoint is up (or failed).
struct RemoteServing {
    std::expected<std::shared_ptr<remote::RemoteHttp>, std::string> result;
};
// A harness's built in tools were read.
struct HarnessTools {
    std::string harness;
    std::expected<std::vector<vp::ToolInfo>, std::string> result;
};
// A server's status or tools changed.
struct ServersChanged {};
// A remote host question arrived or went away.
struct PromptsChanged {};
// The `.claude`, `.cursor` and `.codex` configs were read.
struct Scanned {
    std::vector<mcp::import::Found> found;
    std::vector<std::string> errors;
};
struct Imported {
    std::expected<mcp::import::ImportReport, std::string> result;
};
struct Audit {
    std::string host;
    std::expected<std::vector<vp::AuditRow>, std::string> result;
};
// A server action finished: a notice, or the error.
struct Done {
    std::expected<std::optional<std::string>, std::string> result;
};
struct Terminal {
    terminal::Message message;
};
} // namespace msg

// `to::tools::Open` shows the Tools tab (the sidebar's Tools button);
// `to::tools::AnswerDecision` answers a remote host question's card.
using Message = std::variant<msg::Panel, to::tools::Open, to::tools::AnswerDecision, msg::ServersOpened, msg::RemoteServing,
                             msg::HarnessTools, msg::ServersChanged, msg::PromptsChanged, msg::Scanned, msg::Imported,
                             msg::Audit, msg::Done, msg::Terminal>;

class State : public TypedArea<State> {
  public:
    static constexpr std::string_view ID = area_id::TOOLS;
    using Message = tools::Message;

    static std::pair<std::unique_ptr<State>, Task> make(Ctx& ctx);
    static std::span<const AreaCommand<State>> COMMANDS();

    State(Ctx& ctx, Remote remote, SshPaths ssh);

    Task update_typed(Message message, Ctx& ctx);
    [[nodiscard]] std::vector<PanelTab> panel_tabs() const override;
    [[nodiscard]] QWidget* view_tab(std::string_view id, const Ctx& ctx, const Emit& send) const override;
    void contribute(contracts::SessionConfig& config, const Target& target, const Ctx& ctx) const override;
    [[nodiscard]] std::optional<HostToolSet> host_tools(const contracts::SessionConfig& config, const Target& target,
                                                        const Ctx& ctx) const override;
    Task on_session_event(const SessionEvent& event, Ctx& ctx) override;
    Task project_activated(std::string_view project, const std::filesystem::path& cwd, Ctx& ctx) override;
    [[nodiscard]] Subscriptions subscription() const override;

    [[nodiscard]] const panel_tools::State& panel() const noexcept { return panel_; }
    [[nodiscard]] const terminal::State& shell() const noexcept { return terminal_; }
    [[nodiscard]] const Remote& remote() const noexcept { return remote_; }
    [[nodiscard]] Remote& remote() noexcept { return remote_; }
    [[nodiscard]] const std::shared_ptr<Servers>& servers() const noexcept { return servers_; }
    [[nodiscard]] const std::string& project() const noexcept { return project_; }

    // Rebuilds the `remote` server's hosts from the central config and `~/.ssh/config`, and lists them.
    void reload_hosts(Ctx& ctx);

  private:
    static settings::app::ProjectSettings settings(std::string_view project, const Ctx& ctx);
    static std::optional<std::string> harness(const Ctx& ctx);
    static std::optional<std::filesystem::path> omp(const Ctx& ctx);
    static std::optional<contracts::McpServerSpec> bridge_spec(const contracts::SessionConfig& config,
                                                               const settings::app::ProjectSettings& settings, const Ctx& ctx);

    Task refresh_builtin(const Ctx& ctx);
    Task follow_builtin(const Ctx& ctx);
    [[nodiscard]] vp::McpServer bridge_row(const Ctx& ctx) const;
    [[nodiscard]] vp::McpServer remote_row() const;
    void refresh_servers(Ctx& ctx);
    void refresh_peers(const std::vector<vp::McpServer>& servers, Ctx& ctx);
    void refresh_projects(const Ctx& ctx);
    // Changes the Remote hosts table of the central config with `change` (re-read right before
    // saving), then reloads the hosts.
    void edit_hosts(Ctx& ctx,
                    const std::function<std::expected<std::optional<std::string>, std::string>(
                        std::map<std::string, settings::app::RemoteHost>&)>& change);
    void set_tools(const std::string& project, const std::vector<vp::ToolId>& ids, bool enabled, Ctx& ctx);
    std::shared_ptr<Servers> servers_or_notice(Ctx& ctx) const;
    Task show(panel_tools::Section section, Ctx& ctx);
    Task open(Ctx& ctx);
    Task open_shell(std::string host, Ctx& ctx);
    Task panel_event(vp::ToolsIntent event, Ctx& ctx);
    // The terminal task as app work: its PTY operations on the executor, its clipboard ones here.
    Task terminal_task(terminal::Task task);

    panel_tools::State panel_;
    std::shared_ptr<mcp::Manager> mcp_;
    // The servers of the central config, once loaded.
    std::shared_ptr<Servers> servers_;
    Remote remote_;
    SshPaths ssh_;
    // Built in tools per harness, as read (or why they couldn't be).
    std::map<std::string, std::expected<std::vector<vp::ToolInfo>, std::string>, std::less<>> harness_tools_;
    // omp's tools are being read.
    bool omp_loading_ = false;
    // The harness and app tools revision the Built in section shows.
    std::pair<std::optional<std::string>, std::uint64_t> shown_{std::nullopt, 0};
    // Entries of the last import scan.
    std::vector<mcp::import::Found> found_;
    // Project the toggles apply to.
    std::string project_;
    terminal::State terminal_;
};

} // namespace loa::app::areas::tools
