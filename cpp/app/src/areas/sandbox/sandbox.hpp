#pragma once

// Sandbox: `sandbox_task`. Every session on a harness that takes host tools gets the tool from
// `SandboxService`, with the providers this machine can run; the Sandbox tab lists the runs
// (with each run's patch as a diff to apply) and the providers' settings (port of
// `loa-app/src/areas/sandbox`).

#include <cstdint>
#include <deque>
#include <expected>
#include <filesystem>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/app/adapters/sandbox.hpp"
#include "loa/app/area.hpp"
#include "loa/app/routes.hpp"
#include "loa/sandbox/provider.hpp"
#include "loa/sandbox/runs.hpp"
#include "loa/sandbox/service.hpp"
#include "loa/sandbox/tool.hpp"
#include "loa/ui/kit/widgets/form.hpp"
#include "loa/ui/panel/diff.hpp"

namespace loa::app::areas::sandbox {

namespace sb = loa::sandbox;
namespace diff = loa::ui::panel::diff;

// Id of the Sandbox panel tab.
inline constexpr std::string_view TAB = "sandbox";
// The settings file under the data directory.
inline constexpr std::string_view SETTINGS_FILE = "sandbox.json";

// What the Sandbox tab shows.
enum class View : std::uint8_t { Runs, Settings };

namespace msg {
// A run started, finished or failed.
struct Event {
    sb::SandboxEvent event;
};
// What the providers' probe found.
struct Probed {
    std::vector<sb::ProviderStatus> statuses;
};
// Probe the providers again.
struct Refresh {};
// Show the runs or the settings.
struct Show {
    View view;
};
struct Select {
    std::uint64_t run = 0;
};
struct Diff {
    diff::Message message;
};
// Apply the run's patch to its workspace.
struct Apply {
    std::uint64_t run = 0;
};
struct Applied {
    std::uint64_t run = 0;
    std::expected<void, std::string> result;
};
// Show the worker session of a run.
struct OpenWorker {
    std::string worker;
};
// A text setting, by field key, was edited.
struct Edit {
    std::string key;
    std::string value;
};
struct Network {
    bool on = false;
};
struct Save {};
// A peer's sandbox run on this device failed.
struct PeerFailed {
    adapters::Served served;
};
} // namespace msg

using Message = std::variant<msg::Event, msg::Probed, msg::Refresh, msg::Show, msg::Select, msg::Diff, msg::Apply, msg::Applied,
                             msg::OpenWorker, msg::Edit, msg::Network, msg::Save, msg::PeerFailed>;

// A provider's text settings as typed.
class Section final : public ui::kit::widgets::Form {
  public:
    sb::SandboxKind kind = sb::SandboxKind::Local;
    std::vector<ui::kit::widgets::Field> field_list;
    std::map<std::string, std::string, std::less<>> values;

    [[nodiscard]] std::span<const ui::kit::widgets::Field> fields() const override { return field_list; }
    [[nodiscard]] std::string get(std::string_view key) const override;
    std::expected<void, std::string> set(std::string_view key, std::string_view value) override;
};

// The settings form over a config: a section per provider, and the network switch.
struct Draft {
    std::vector<Section> sections;
    bool network = false;

    [[nodiscard]] static Draft of(const sb::SandboxConfig& config);
    [[nodiscard]] sb::SandboxConfig config() const;
    void edit(std::string_view key, std::string_view value);
};

// How the UI names a provider.
[[nodiscard]] std::string_view kind_label(sb::SandboxKind kind);

// Peers' failed runs on this device, queued from any thread for the subscription.
struct Failures {
    std::mutex mutex;
    std::deque<adapters::Served> queue;
};

class State : public TypedArea<State> {
  public:
    static constexpr std::string_view ID = area_id::SANDBOX;
    using Message = sandbox::Message;

    static std::pair<std::unique_ptr<State>, Task> make(Ctx& ctx);
    static std::span<const AreaCommand<State>> COMMANDS();

    Task update_typed(Message message, Ctx& ctx);
    [[nodiscard]] std::vector<PanelTab> panel_tabs() const override;
    [[nodiscard]] QWidget* view_tab(std::string_view id, const Ctx& ctx, const Emit& send) const override;
    [[nodiscard]] std::optional<HostToolSet> host_tools(const contracts::SessionConfig& config, const Target& target,
                                                        const Ctx& ctx) const override;
    Task on_session_event(const SessionEvent& event, Ctx& ctx) override;
    [[nodiscard]] Subscriptions subscription() const override;

    // The runs, oldest first.
    [[nodiscard]] const sb::RunLog& runs() const noexcept { return runs_; }
    [[nodiscard]] View view() const noexcept { return view_; }
    [[nodiscard]] const Draft& draft() const noexcept { return draft_; }
    [[nodiscard]] const sb::SandboxService& service() const noexcept { return *service_; }
    [[nodiscard]] std::optional<std::uint64_t> selected() const noexcept { return selected_; }
    [[nodiscard]] const diff::State& patch() const noexcept { return diff_; }

  private:
    Task probe();
    // Lists `sandbox_task` among the app tools with one row per provider.
    void publish(Ctx& ctx) const;
    Task event(sb::SandboxEvent event, Ctx& ctx);
    Task select(std::uint64_t run, Ctx& ctx);
    Task apply(std::uint64_t run, Ctx& ctx);
    // Saves the settings, hands them to the service for new sessions and probes again.
    Task save(Ctx& ctx);
    void show_settings(Ctx& ctx);
    // What the empty run list says: where runs come from and which providers can take them.
    [[nodiscard]] std::string empty_text() const;

    [[nodiscard]] QWidget* draw(const Ctx& ctx, const Emit& send) const;
    [[nodiscard]] QWidget* runs_view(const Ctx& ctx, const Emit& send) const;
    [[nodiscard]] QWidget* run_row(const sb::RunEntry& entry, const Ctx& ctx, const Emit& send) const;
    [[nodiscard]] QWidget* detail(const sb::RunEntry& entry, const Emit& send) const;
    [[nodiscard]] QWidget* settings_view(const Emit& send) const;
    [[nodiscard]] QWidget* provider(sb::SandboxKind kind, QWidget* settings) const;

    std::shared_ptr<sb::SandboxService> service_;
    // Every run event since the area started, for the run feed.
    std::shared_ptr<sb::SandboxEvents::Receiver> events_;
    // Failures of peers' runs on this device, for the failure feed.
    std::shared_ptr<Failures> served_;
    sb::RunLog runs_;
    // The last probe; none until it answers.
    std::optional<std::vector<sb::ProviderStatus>> statuses_;
    bool probing_ = false;
    View view_ = View::Runs;
    std::optional<std::uint64_t> selected_;
    // The selected run's patch.
    diff::State diff_;
    std::optional<std::uint64_t> applying_;
    Draft draft_;
    std::filesystem::path settings_;
};

} // namespace loa::app::areas::sandbox
