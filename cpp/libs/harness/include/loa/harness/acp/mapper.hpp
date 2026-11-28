#pragma once

// ACP traffic as session DOM writes (port of `acp/mapper.rs`): chunks become messages, tool
// calls `<call>`s, plans `<todo>`s, permission requests approval `<decision>`s; anything
// else is kept as `<ext harness="acp">`.

#include <expected>
#include <map>
#include <optional>
#include <string>
#include <string_view>
#include <tuple>
#include <utility>
#include <variant>
#include <vector>

#include "loa/harness/acp/wire.hpp"
#include "loa/harness/kit/mapper.hpp"

namespace loa::harness::acp {

using Json = loa::runtime::json::Value;

// Session attribute holding the agent's own mode id.
inline constexpr std::string_view X_MODE = "x-acp-mode";
// Session attribute naming the agent program (`agentInfo`).
inline constexpr std::string_view X_AGENT = "x-acp-agent";
// Decision attributes keeping the offered ACP option ids and kinds.
inline constexpr std::string_view X_OPTION_IDS = "x-acp-option-ids";
inline constexpr std::string_view X_OPTION_KINDS = "x-acp-option-kinds";
// Decision attribute holding the option id that was sent back.
inline constexpr std::string_view X_OPTION = "x-acp-option";
// Turn attribute holding a stop reason other than `end_turn`.
inline constexpr std::string_view X_STOP_REASON = "x-acp-stop-reason";
// Todo attribute holding the plan entry priority.
inline constexpr std::string_view X_PRIORITY = "x-acp-priority";

// What the session feeds the `AcpMapper`.
namespace input {
// The user's prompt, sent as `session/prompt` request `id`; opens the turn.
struct Prompt { std::string id; loa::contracts::Prompt prompt; };
// The agent's response to request `id`; the error holds the failure.
struct Response { std::string id; std::expected<Json, std::string> result; };
// `session/load` request `id` replays history the DOM already has.
struct Load { std::string id; };
struct Interrupt {};
// The agent is gone; `shutdown` when the app closed it.
struct Closed { bool shutdown = false; };
// The params of a `session/update`.
struct Update { Json params; };
// A `session/request_permission` request.
struct Permission { Json id; Json params; };
// The answer to the permission request `request`.
struct Answer { Json request; loa::contracts::Answer answer; };
} // namespace input
using Input = std::variant<input::Prompt, input::Response, input::Load, input::Interrupt, input::Closed, input::Update,
                           input::Permission, input::Answer>;

// A fixture line: `{"$":"send"|"result"|"interrupt"|"closed"|"load"|"loaded"|"answer",…}` or
// a JSON-RPC message from the agent. The prompt is request `1`, the load `99`.
[[nodiscard]] Input decode_fixture(Json raw);

// What the mapper has the session send to the agent.
namespace reply {
// The result of the agent's request `id`.
struct Respond { Json id; Json result; friend bool operator==(const Respond&, const Respond&) = default; };
// A JSON-RPC "invalid params" error for the agent's request `id`.
struct Reject { Json id; std::string message; friend bool operator==(const Reject&, const Reject&) = default; };
// `session/cancel`.
struct Cancel { friend bool operator==(const Cancel&, const Cancel&) = default; };
} // namespace reply
using Reply = std::variant<reply::Respond, reply::Reject, reply::Cancel>;

// The DOM side of one ACP session.
class AcpMapper final : public kit::EventMapper<Input, Reply> {
  public:
    // Writes under the `<session>` `root`; assistant messages carry `model`.
    AcpMapper(core::NodeId root, std::optional<std::string> model);

    contracts::Result<std::vector<Reply>> apply(Input input, const core::DomWriter& w) override;
    contracts::Result<std::optional<Reply>> answer(core::NodeId decision, const contracts::Answer& answer,
                                                   const core::DomWriter& w) override;
    [[nodiscard]] const kit::TurnLedger<std::string>& ledger() const override { return ledger_; }
    kit::TurnLedger<std::string>& ledger_mut() override { return ledger_; }

    // Takes over the todos and mode a resumed session already has.
    void adopt(const core::Dom& dom);
    [[nodiscard]] core::NodeId root() const { return ledger_.session; }
    // The agent's session id is `id`.
    std::expected<void, core::CommitError> set_native(const core::DomWriter& w, std::string_view id);
    // Tests replay one agent session; this names it.
    void adopt_native(std::string_view id) { native_ = std::string(id); }
    // The agent switched to `model`: later assistant messages carry it.
    std::expected<void, core::CommitError> model_changed(const core::DomWriter& w, std::string_view model);
    [[nodiscard]] std::optional<std::string_view> mode_id() const;
    // Records the agent's current mode on the session.
    std::expected<void, core::CommitError> mode_changed(const core::DomWriter& w, std::string_view mode_id);

  private:
    using Out = std::expected<void, core::CommitError>;
    struct Stop { wire::StopReason reason; };
    struct Failed { std::string message; };
    struct Gone { bool shutdown; };
    using TurnEnd = std::variant<Stop, Failed, Gone>;
    struct Call {
        core::NodeId node;
        std::optional<core::NodeId> input;
        core::CallStatus status;
        std::vector<Json> content;
        std::optional<Json> raw_output;
    };
    struct Turn {
        std::optional<std::tuple<core::NodeId, core::MessageKind, std::optional<std::string>>> message;
        // In insertion order.
        std::vector<std::pair<std::string, Call>> calls;
        // The `session/prompt` request whose response ends the turn.
        std::optional<std::string> prompt;
        bool interrupted = false;
        Call* call(std::string_view id);
    };
    struct Todo {
        core::NodeId node;
        std::string text;
        core::TodoState state;
        std::optional<std::string> priority;
    };
    struct Pending {
        Json rpc_id;
        std::vector<wire::PermissionOption> options;
    };

    [[nodiscard]] std::optional<core::NodeId> turn_node() const;
    std::expected<std::vector<Reply>, core::CommitError> end_turn(const core::DomWriter& w, TurnEnd end);
    std::expected<std::vector<Reply>, core::CommitError> cancel_decisions(const core::DomWriter& w);
    Out update(const core::DomWriter& w, Json update);
    Out chunk(const core::DomWriter& w, core::MessageKind kind, std::string_view update_kind, const Json& update);
    std::expected<std::optional<core::NodeId>, core::CommitError> tool_call(const core::DomWriter& w, wire::ToolCallFields fields);
    Out plan(const core::DomWriter& w, wire::Plan plan);
    Out permission(const core::DomWriter& w, Json rpc_id, wire::RequestPermissionRequest request);
    Out ext_here(const core::DomWriter& w, std::string_view kind, std::string raw) const;

    kit::TurnLedger<std::string> ledger_;
    Turn turn_;
    std::optional<std::string> model_;
    std::optional<std::string> native_;
    std::optional<std::string> mode_id_;
    std::vector<Todo> todos_;
    std::map<core::NodeId, Pending> decisions_;
    // A running `session/load`, whose replayed history is skipped.
    std::optional<std::string> replay_;
};

// The offered option `value` names: its id, its label, or a `loa::core::answer` constant
// matched by option kind.
[[nodiscard]] const wire::PermissionOption* pick_option(const std::vector<wire::PermissionOption>& options,
                                                        std::string_view value);

// `value` with every object's keys sorted, recursively (serde's `sort_all_objects`).
void sort_all_objects(Json& value);

} // namespace loa::harness::acp
