#pragma once

// Workflow settings and the composer's workflow choices (port of
// `loa-app/src/areas/workflows/mod.rs`).

#include <expected>
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
#include "loa/models/service.hpp"
#include "loa/settings/models/workflows.hpp"
#include "loa/ui/settings/workflows/state.hpp"
#include "loa/view/settings_workflows.hpp"

namespace loa::app::areas::workflows {

namespace screen = loa::ui::settings::workflows;
namespace shown = loa::view::settings::workflows;
namespace domain = loa::settings::models::workflows;
using Page = to::models::Page;

// What a read of the workflow files is for.
struct Load {
    enum class Kind : std::uint8_t { Refresh, Open, New, Edit };
    Kind kind = Kind::Refresh;
    // `Edit`: the role to open.
    std::string role;
};

// The workflows, the snapshot guarding the next edit, and the agents the editor offers.
struct Data {
    std::vector<domain::Workflow> workflows;
    domain::WorkflowSnapshot snapshot;
    std::vector<shown::AgentInfo> agents;
};

namespace msg {
using Open = to::workflows::Open;
using Pick = to::workflows::Pick;
struct New {};
struct Edit {
    std::string role;
};
struct ShowPage {
    Page page;
};
struct Screen {
    screen::Message message;
};
struct Loaded {
    Load load;
    std::expected<Data, std::string> result;
};
struct Saved {
    std::optional<std::string> role;
    std::expected<void, shown::Error> result;
};
} // namespace msg

using Message = std::variant<msg::Open, msg::New, msg::Edit, msg::Pick, msg::ShowPage, msg::Screen, msg::Loaded, msg::Saved>;

class State final : public TypedArea<State>, public WorkflowsPort {
  public:
    static constexpr std::string_view ID = area_id::WORKFLOWS;
    using Message = workflows::Message;

    [[nodiscard]] static std::pair<std::unique_ptr<State>, Task> make(Ctx& ctx);
    [[nodiscard]] static std::span<const AreaCommand<State>> COMMANDS();

    Task update_typed(Message message, Ctx& ctx);
    void sync(Ctx& ctx) const override;
    void contribute(contracts::SessionConfig& config, const Target& target, const Ctx& ctx) const override;
    [[nodiscard]] QWidget* view_screen(const Ctx& ctx, const Emit& send) const override;
    Task on_session_event(const SessionEvent& event, Ctx& ctx) override;

    [[nodiscard]] const std::vector<domain::Workflow>& workflows() const noexcept { return workflows_; }
    [[nodiscard]] const screen::State& screen() const noexcept { return screen_; }
    // The status line shown in place of the editor while a read or write runs.
    [[nodiscard]] std::optional<std::string_view> busy() const noexcept { return busy_; }

    explicit State(models::Backend backend) : backend_(std::move(backend)) {}

  private:
    [[nodiscard]] Task read(const Ctx& ctx, Load load) const;
    Task open(Load load, Ctx& ctx);
    Task reload(const Ctx& ctx, Load load);
    void pick(std::string_view role, Ctx& ctx) const;
    Task intent(shown::Intent intent, Ctx& ctx);
    Task loaded(msg::Loaded loaded, Ctx& ctx);

    models::Backend backend_;
    screen::State screen_;
    std::vector<domain::Workflow> workflows_;
    std::optional<domain::WorkflowSnapshot> snapshot_;
    std::optional<std::string_view> busy_;
};

} // namespace loa::app::areas::workflows
