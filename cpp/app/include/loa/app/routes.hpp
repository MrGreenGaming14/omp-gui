#pragma once

// What the app shell hands to specific areas, and asks of them (the `areas::<area>::Message`
// variants and `areas.get::<State>()` calls of `loa-app/src/app.rs`). Each area accepts the
// `to::<area>` messages addressed to it as alternatives of its own message variant, and
// implements the port the app asks of it; the app finds ports through `Registry::port`.

#include <expected>
#include <optional>
#include <string>
#include <string_view>
#include <utility>

#include "loa/app/area.hpp"
#include "loa/app_runtime/session.hpp"
#include "loa/core/patch.hpp"
#include "loa/view/session.hpp"

class QWidget;

namespace loa::app {

// The area ids, in the order the app calls them (`areas/mod.rs` `AREAS`).
namespace area_id {
inline constexpr std::string_view HARNESS = "harness";
inline constexpr std::string_view SESSIONS = "sessions";
inline constexpr std::string_view DECISIONS = "decisions";
inline constexpr std::string_view TOOLS = "tools";
inline constexpr std::string_view SURFACES = "surfaces";
inline constexpr std::string_view MODELS = "models";
inline constexpr std::string_view PROJECTS = "projects";
inline constexpr std::string_view PEERS = "peers";
inline constexpr std::string_view UPDATER = "updater";
inline constexpr std::string_view SANDBOX = "sandbox";
inline constexpr std::string_view AGENTS = "agents";
inline constexpr std::string_view WORKFLOWS = "workflows";
} // namespace area_id

namespace to {
namespace harness {
struct Pick {
    std::string id;
};
} // namespace harness
namespace sessions {
struct Delete {
    std::string id;
};
struct Mode {
    loa::view::session::SessionMode mode;
};
struct Chat {
    loa::view::session::transcript::Intent intent;
};
// Answer to a plan review the app raised, from the decision card (the decisions area's).
struct AnswerPlan {
    SessionId session;
    core::NodeId decision;
    contracts::Answer answer;
};
} // namespace sessions
namespace decisions {
// An answer to `decision` finished; an error reopens its card with the error shown.
struct Answered {
    std::string session;
    core::NodeId decision;
    std::expected<void, std::string> result;
};
} // namespace decisions
namespace tools {
struct Open {};
// Answer to a remote host approval or host key question, from its decision card (the decisions
// area's): the `remote` server owns these decisions, not the harness.
struct AnswerDecision {
    SessionId session;
    core::NodeId decision;
    contracts::Answer answer;
};
} // namespace tools
namespace models {
// The models screens (`loa_ui_settings::models::Page`).
enum class Page : std::uint8_t { SignIn, Model, Settings, Workflows };
struct Model {
    loa::view::session::composer::ModelOption model;
};
struct Action {
    std::string id;
};
struct Effort {
    std::string effort;
};
struct Show {
    Page page;
};
// The models screen's own close button.
struct Close {};
// Use the model role `role`: the composer's model follows it (the workflows area's pick).
struct PickRole {
    std::string role;
};
} // namespace models
namespace projects {
struct Open {
    std::string project;
};
struct New {};
struct NewChatIn {
    std::string project;
};
struct AskDelete {
    std::string project;
};
struct Move {
    std::string session;
    std::optional<std::string> project;
};
struct Activate {
    std::string project;
};
} // namespace projects
namespace decisions {
// A peer answered a decision of session `session` on this node (the decisions area's `Answer`).
using Answer = loa::view::session::decisions::desk_intent::Answer;
} // namespace decisions
namespace peers {
struct OpenRecent {
    std::string id;
};
struct Refresh {};
// The sidebar's Peers rows.
struct Open {};
} // namespace peers
namespace updater {
struct Open {};
} // namespace updater
namespace agents {
struct Open {};
} // namespace agents
namespace workflows {
struct Pick {
    std::string role;
};
struct Open {};
} // namespace workflows
} // namespace to

namespace peers {
// The id prefix of a peer's session listed in the sidebar.
inline constexpr std::string_view RECENT_PREFIX = "peer:";
// Whether the sidebar session `id` is a peer's.
[[nodiscard]] inline bool is_remote(std::string_view id) { return id.starts_with(RECENT_PREFIX); }
} // namespace peers

namespace tools {
// The `request_id` prefix of the decisions the tools area's `remote` server raises.
inline constexpr std::string_view REMOTE_REQUEST_PREFIX = "remote:";
// Whether `decision` is a remote host approval or host key question of the `remote` server.
[[nodiscard]] bool is_remote_decision(const core::Dom& dom, core::NodeId decision);
} // namespace tools

// What the app asks of the sessions area.
class SessionsPort {
  public:
    virtual ~SessionsPort() = default;
    // Whether a role switch due for session `id` holds its queued prompts.
    virtual bool hold(std::string_view id) = 0;
    // A streamed append reached session `id`.
    virtual void on_append(const SessionId& id, const core::Patch& patch, Ctx& ctx) = 0;
    // The subagent strip over the center and the child session it shows, when one is picked.
    [[nodiscard]] virtual std::optional<std::pair<QWidget*, QWidget*>> view_child_center(const Ctx& ctx,
                                                                                         const Emit& send) const = 0;
};

// What the app asks of the decisions area.
class DecisionsPort {
  public:
    virtual ~DecisionsPort() = default;
    // Whether a decision card has focus.
    [[nodiscard]] virtual bool focused() const = 0;
    // The message that dismisses the focused card.
    [[nodiscard]] virtual Message dismiss() const = 0;
    // The decision card at `node` of session `session`; null when it is gone.
    [[nodiscard]] virtual QWidget* card(std::string_view session, core::NodeId node, const Emit& send) const = 0;
    // The message a key press no widget captured sends to the focused card (number keys and
    // Enter); `nullopt` when no card takes it. Esc is the app's (`dismiss`).
    [[nodiscard]] virtual std::optional<Message> shortcut(const ui::shell::palette::KeyPress& key) const = 0;
};

// What the app asks of the models area.
class ModelsPort {
  public:
    virtual ~ModelsPort() = default;
    // Reads the model catalog once the main window is open.
    [[nodiscard]] virtual Task read_catalog() = 0;
    // The body of a models screen, drawn inside Configs or Settings.
    [[nodiscard]] virtual QWidget* page_body(to::models::Page page, const Emit& send) const = 0;
};

// What the app asks of the projects area.
class ProjectsPort {
  public:
    virtual ~ProjectsPort() = default;
    // The project whose page the center shows.
    [[nodiscard]] virtual std::optional<std::string> shown() const = 0;
};

// What the app asks of the workflows area.
class WorkflowsPort {
  public:
    virtual ~WorkflowsPort() = default;
    // Brings the composer's workflow picker in line with the models and harness.
    virtual void sync(Ctx& ctx) const = 0;
};

// What the app asks of the agents area.
class AgentsPort {
  public:
    virtual ~AgentsPort() = default;
    // The Agents section of Configs; null to list the definition files instead.
    [[nodiscard]] virtual QWidget* configs_body(const Ctx& ctx, const Emit& send) const = 0;
};

} // namespace loa::app
