#pragma once

// Agents: the form that builds an omp agent definition (port of `loa-app/src/areas/agents`).
//
// The screen comes from `loa::ui::settings::agents`; Configs › Agents draws it, and the palette's
// New agent and Edit agent commands open it in the center. Opening it resolves omp's agent
// directory through the model backend, lists the user agents there and the project agents
// under the active folder, and reads what the form offers: omp's models and role aliases, and
// omp's tools (the built in defaults when omp can't list them, which the screen says).
// Everything here is for omp sessions: the palette lists the commands only while the harness
// is omp.
//
// Save checks the form with `agents::validate` and writes the definition: back to the file
// the form edits, or to a new file in the user agents directory. An existing name is replaced
// only after the screen asks. Enable, disable and delete act on the file being edited (delete
// asks first). Each outcome refreshes the lists and says that only omp sessions started later
// see the change.

#include <expected>
#include <filesystem>
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
#include "loa/settings/models/agents.hpp"
#include "loa/settings/models/error.hpp"
#include "loa/ui/settings/agents/state.hpp"
#include "loa/view/settings_agents.hpp"

namespace loa::app::areas::agents {

namespace screen = loa::ui::settings::agents;

// Shown when the harness isn't omp.
inline constexpr std::string_view OMP_ONLY = "Available with OMP sessions";

// What a read is for.
struct Load {
    enum class Kind : std::uint8_t {
        // Only list the agents, for the palette.
        Refresh,
        New,
        Edit,
    };
    Kind kind = Kind::Refresh;
    // `Edit`: the agent's name.
    std::string name;

    friend bool operator==(const Load&, const Load&) = default;
};

// What a read found.
struct Data {
    std::filesystem::path agent_dir;
    std::vector<settings::models::agents::AgentFile> files;
    // Files that couldn't be read, with the reason.
    std::vector<std::string> broken;
    // Models and tools; not read for `Load::Kind::Refresh`.
    std::optional<loa::view::settings::agents::Choices> choices;
};

namespace msg {
// Opens the screen on the agent with this name.
struct Edit {
    std::string name;
};
struct Screen {
    screen::Message message;
};
struct Loaded {
    Load load;
    std::expected<Data, std::string> result;
};
} // namespace msg

// `to::agents::Open` opens the screen on a blank form.
using Message = std::variant<to::agents::Open, msg::Edit, msg::Screen, msg::Loaded>;

class State : public TypedArea<State>, public AgentsPort {
  public:
    static constexpr std::string_view ID = area_id::AGENTS;
    using Message = agents::Message;

    static std::pair<std::unique_ptr<State>, Task> make(Ctx& ctx);
    static std::span<const AreaCommand<State>> COMMANDS();

    Task update_typed(Message message, Ctx& ctx);
    [[nodiscard]] QWidget* view_screen(const Ctx& ctx, const Emit& send) const override;
    // The form, for Configs › Agents: only with omp, once it has loaded.
    [[nodiscard]] QWidget* configs_body(const Ctx& ctx, const Emit& send) const override;

    // Lists omp's tools as `tools` instead of asking omp; for tests.
    void use_tools(std::vector<std::string> tools) { fixed_tools_ = std::move(tools); }
    [[nodiscard]] const screen::State& form() const noexcept { return screen_; }

  private:
    using AgentDefinition = settings::models::agents::AgentDefinition;
    using Draft = loa::view::settings::agents::Draft;
    template <typename T = void>
    using Result = settings::models::Result<T>;

    static bool omp_active(const Ctx& ctx);
    // Starts reading for `load`.
    [[nodiscard]] Task read(const Ctx& ctx, Load load) const;
    // Shows the screen and reads for `load`, unless the harness isn't omp.
    Task open(Load load, Ctx& ctx);
    void loaded(const Load& load, std::expected<Data, std::string> result, Ctx& ctx);
    void screen_intent(loa::view::settings::agents::Intent intent, Ctx& ctx);
    // The definition `draft` describes: its fields over the listed file it started from.
    [[nodiscard]] Result<AgentDefinition> definition(const Draft& draft) const;
    // The app's checks over the form.
    [[nodiscard]] std::vector<loa::view::settings::agents::FieldError> check(const Draft& draft) const;
    void save(Draft draft, Ctx& ctx);
    void replace(const Draft& draft, const std::filesystem::path& path, Ctx& ctx);
    [[nodiscard]] std::filesystem::path user_agents() const;
    void replace_file(const AgentDefinition& definition, const std::filesystem::path& path, Ctx& ctx);
    [[nodiscard]] Result<std::filesystem::path> overwrite(const AgentDefinition& definition, const std::filesystem::path& path) const;
    void saved(const AgentDefinition& definition, Result<std::filesystem::path> result, Ctx& ctx);
    // Keeps the form open with the reason next to the field it concerns.
    void show_error(const settings::models::Error& error);
    void toggle(const std::filesystem::path& path, Ctx& ctx);
    void remove(const std::filesystem::path& path, Ctx& ctx);
    // Lists the agents again, for the screen and the palette.
    void relist(const Ctx& ctx);
    // Each agent's name once, in list order.
    [[nodiscard]] std::vector<std::string> names() const;
    [[nodiscard]] QWidget* draw(bool titled, const Emit& send) const;

    screen::State screen_;
    // omp's agent directory, once resolved.
    std::optional<std::filesystem::path> agent_dir_;
    std::vector<settings::models::agents::AgentFile> files_;
    // The tools the form offers, checked by Save.
    std::vector<std::string> tools_;
    // A full read finished: the form has its choices.
    bool ready_ = false;
    // The tools used instead of asking omp; set by tests.
    std::optional<std::vector<std::string>> fixed_tools_;
};

} // namespace loa::app::areas::agents
