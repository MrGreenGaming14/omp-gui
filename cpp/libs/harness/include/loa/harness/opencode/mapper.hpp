#pragma once

// OpenCode server events (and export files) onto the session DOM: one `<turn id=<user message
// id>>` per user message; text and reasoning parts as `<message id=<part id>>`; tool parts as
// `<call>` (the task tool's child session as `<job kind="subagent">`); assistant message info
// and every other part as `<ext>`; permission and question requests as `<decision>`; the todo
// list as `<todo>`; a finished summary message as `<compaction>`. Native fields no attribute
// holds are kept as JSON in `x-oc` for the export.

#include <cstdint>
#include <map>
#include <optional>
#include <string>
#include <string_view>
#include <unordered_map>
#include <variant>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/harness/kit/ledger.hpp"
#include "loa/harness/kit/mapper.hpp"
#include "loa/harness/opencode/event.hpp"
#include "loa/runtime/json.hpp"

namespace loa::harness::opencode {

// Harness id written to `<session harness>` and `<ext harness>`.
inline constexpr std::string_view HARNESS = "opencode";

// Extension attributes this adapter writes.
namespace xattr {
// Compact JSON of the native object's fields no other attribute holds.
inline constexpr std::string_view OC = "x-oc";
// Native id of the message an element belongs to.
inline constexpr std::string_view MESSAGE = "x-message";
// Native part id of a `<call>` or `<ext type="part">`.
inline constexpr std::string_view PART = "x-part";
// Native id of a subagent's child session, on its `<job>`.
inline constexpr std::string_view NATIVE_SESSION = "x-native-session";
inline constexpr std::string_view PRIORITY = "x-priority";
// Todo status that isn't one of the schema states.
inline constexpr std::string_view STATUS = "x-status";
// Position of a question within its request.
inline constexpr std::string_view INDEX = "x-index";
// A question that accepts typed answers.
inline constexpr std::string_view CUSTOM = "x-custom";
// JSON of the session's pending revert.
inline constexpr std::string_view REVERT = "x-revert";
inline constexpr std::string_view REASONING_TOKENS = "x-reasoning-tokens";
} // namespace xattr

// `<ext type>` values this adapter writes.
namespace ext {
inline constexpr std::string_view MESSAGE = "message", PART = "part", ERROR = "error";
}

// Session fields the root's own attributes hold.
inline constexpr std::string_view SESSION_FIELDS[] = {"/id", "/title", "/directory", "/time/created"};

// What goes on the `<session>` root.
struct RootInfo {
    std::string session_id, node;
    std::optional<std::string> cwd, project, model, model_role, effort;
    std::optional<loa::core::SessionMode> mode;
    // The native session object.
    json::Value native;
};

// The request answering a decision: `POST path` with `body`.
struct Reply {
    std::string path;
    std::optional<json::Value> body;
    friend bool operator==(const Reply&, const Reply&) = default;
};

// Maps one native session (and its subagent children) onto one DOM.
class Mapper final : public kit::EventMapper<Event, Reply> {
  public:
    using NodeId = loa::core::NodeId;
    using Result = loa::contracts::Result<>;

    // Inserts the `<session>` root into an empty DOM.
    [[nodiscard]] static loa::contracts::Result<Mapper> start(const loa::core::DomWriter& w, RootInfo info);
    // Rebuilds the mapper's view of a session from its DOM (resume, fork).
    [[nodiscard]] static loa::contracts::Result<Mapper> from_dom(const loa::core::Dom& dom);

    [[nodiscard]] NodeId root() const { return root_; }
    // The native session id events are matched against.
    [[nodiscard]] const std::string& native_id() const { return native_id_; }
    // Points the mapper at another native session (after a native fork).
    [[nodiscard]] Result set_native_id(const loa::core::DomWriter& w, std::string_view id);
    // The model of the latest assistant message or selection, `provider/model`.
    [[nodiscard]] const std::optional<std::string>& model() const { return model_; }
    [[nodiscard]] Result set_model(const loa::core::DomWriter& w, std::string_view model);
    [[nodiscard]] loa::contracts::SessionState state() const { return ledger_.state(); }

    // Inserts the `<turn>` and user `<message>` of a prompt the app sends as native message `message_id`.
    [[nodiscard]] loa::contracts::Result<NodeId> begin_turn(const loa::core::DomWriter& w, std::string_view message_id,
                                                            std::string_view text, std::string_view by);
    // Ends the open turn as failed, keeping `message` in `<ext type="error">`.
    [[nodiscard]] Result fail_turn(const loa::core::DomWriter& w, NodeId turn, std::string_view message);
    // The interrupt: streaming messages, running calls and jobs and waiting decisions are cancelled, then the turn.
    [[nodiscard]] Result cancel_turn(const loa::core::DomWriter& w);
    // Ends the open turn as the harness reported it (an import reached the end of the file).
    [[nodiscard]] Result finish(const loa::core::DomWriter& w);
    // Feeds one server event.
    [[nodiscard]] Result apply_event(const loa::core::DomWriter& w, const Event& event);
    // A user or assistant message was created or changed.
    [[nodiscard]] Result message_updated(const loa::core::DomWriter& w, const json::Value& info);
    // A part was created or changed (always the full part).
    [[nodiscard]] Result part_updated(const loa::core::DomWriter& w, const json::Value& part);
    // The request answering `decision`; `nullopt` while other questions of the request still wait.
    [[nodiscard]] loa::contracts::Result<std::optional<Reply>> plan_reply(NodeId decision,
                                                                          const loa::contracts::Answer& answer) const;
    // Records `answer` on `decision`; the turn runs again once nothing waits.
    [[nodiscard]] Result record_answer(const loa::core::DomWriter& w, NodeId decision, const loa::contracts::Answer& answer);

    loa::contracts::Result<std::vector<Reply>> apply(Event ev, const loa::core::DomWriter& w) override;
    loa::contracts::Result<std::optional<Reply>> answer(NodeId decision, const loa::contracts::Answer& answer,
                                                        const loa::core::DomWriter& w) override;
    [[nodiscard]] const kit::TurnLedger<std::string>& ledger() const override { return ledger_; }
    kit::TurnLedger<std::string>& ledger_mut() override { return ledger_; }

  private:
    struct Usage {
        std::int64_t input = 0, output = 0, reasoning = 0, cache_read = 0, cache_write = 0;
        double cost = 0;
    };
    struct TurnState {
        std::map<std::string, Usage> steps;
        bool error = false, aborted = false;
        std::int64_t last_ts = 0;
        std::optional<std::string> model;
    };
    struct Msg {
        NodeId turn;
        bool user = false;
        std::optional<NodeId> ext;
        std::string oc;
        std::optional<std::string> model;
        bool summary = false, compacted = false;
    };
    struct Call {
        NodeId node, input;
        std::string input_text;
        std::optional<std::pair<NodeId, std::string>> result;
        loa::core::CallStatus status;
        std::optional<std::string> title;
        std::optional<std::int64_t> started, ended;
        std::optional<NodeId> job;
    };
    struct TextPart {
        NodeId node;
        std::string text;
        loa::core::MessageStatus status;
    };
    struct ExtPart {
        NodeId node;
        std::string text;
    };
    struct Part {
        NodeId turn;
        std::string message, oc;
        std::variant<TextPart, Call, ExtPart> kind;
        [[nodiscard]] NodeId node() const;
    };
    struct Owner {
        NodeId turn;
        bool user;
        std::optional<std::string> model;
    };
    struct Pending {
        std::string request_id;
        bool question = false;
        std::size_t index = 0, count = 0;
        std::optional<NodeId> turn;
        bool waiting = true;
        std::optional<std::vector<std::string>> answer;
    };
    struct Child {
        std::optional<NodeId> job, call;
        // Set while the job runs; routes the child's decisions.
        std::optional<NodeId> turn;
    };
    struct ToolState;
    struct Place {
        NodeId parent;
        std::optional<NodeId> call, turn;
    };

    Mapper(NodeId root, std::optional<std::string> model);
    [[nodiscard]] std::optional<NodeId> active() const;
    void open_turn(std::string_view message_id, NodeId turn, std::int64_t ts);
    void adopt(NodeId turn, std::int64_t ts, std::optional<std::string> model);
    [[nodiscard]] Result close_active(const loa::core::DomWriter& w, std::optional<loa::core::TurnStatus> forced);
    [[nodiscard]] Result session_updated(const loa::core::DomWriter& w, const json::Value& info);
    [[nodiscard]] Result session_error(const loa::core::DomWriter& w, const json::Value& error);
    [[nodiscard]] Result user_message(const loa::core::DomWriter& w, const std::string& id, const json::Value& info);
    [[nodiscard]] Result assistant_message(const loa::core::DomWriter& w, const std::string& id, const json::Value& info);
    [[nodiscard]] Result record_compaction(const loa::core::DomWriter& w, const std::string& message, NodeId turn);
    [[nodiscard]] Result message_removed(const loa::core::DomWriter& w, const std::string& id);
    [[nodiscard]] Result part_delta(const loa::core::DomWriter& w, const std::string& part, std::string_view field,
                                    std::string_view delta);
    [[nodiscard]] Result text_part(const loa::core::DomWriter& w, const std::string& id, const std::string& message,
                                   const json::Value& part, const Owner& owner, bool thinking);
    [[nodiscard]] loa::contracts::Result<NodeId> new_text(const loa::core::DomWriter& w, const std::string& message,
                                                          const json::Value& part, const std::string& oc, const Owner& owner,
                                                          bool thinking, loa::core::MessageStatus status);
    [[nodiscard]] Result tool_part(const loa::core::DomWriter& w, const std::string& id, const std::string& message,
                                   const json::Value& part, NodeId turn);
    [[nodiscard]] Result task_job(const loa::core::DomWriter& w, const std::string& part, NodeId turn, const json::Value& state);
    [[nodiscard]] Result ext_part(const loa::core::DomWriter& w, const std::string& id, const std::string& message,
                                  const json::Value& part, NodeId turn);
    [[nodiscard]] Result sync_todos(const loa::core::DomWriter& w, const std::vector<Todo>& todos);
    [[nodiscard]] Place request_place(const std::optional<ToolRef>& tool, std::optional<std::string_view> child) const;
    [[nodiscard]] Result waiting(const loa::core::DomWriter& w, NodeId node, const std::string& request_id, Pending pending);
    [[nodiscard]] Result permission_asked(const loa::core::DomWriter& w, const PermissionRequest& request,
                                          std::optional<std::string_view> child);
    [[nodiscard]] Result question_asked(const loa::core::DomWriter& w, const QuestionRequest& request,
                                        std::optional<std::string_view> child);
    [[nodiscard]] Result resume_turn(const loa::core::DomWriter& w, std::optional<NodeId> turn) const;
    [[nodiscard]] std::vector<NodeId> waiting_on(std::string_view request_id) const;
    [[nodiscard]] Result permission_replied(const loa::core::DomWriter& w, const std::string& request_id, std::string_view reply);
    [[nodiscard]] Result question_replied(const loa::core::DomWriter& w, const std::string& request_id,
                                          const std::vector<std::vector<std::string>>& answers);
    [[nodiscard]] Result request_closed(const loa::core::DomWriter& w, const std::string& request_id);
    [[nodiscard]] Result child_event(const loa::core::DomWriter& w, const std::string& child, const Event& event);
    void index_turn(const loa::core::Dom& dom, const loa::core::Node& turn);
    void index_decision(const loa::core::Node& node, NodeId turn);

    NodeId root_;
    std::string native_id_, session_oc_;
    std::optional<std::string> title_, revert_;
    kit::TurnLedger<std::string> ledger_;
    // State of the ledger's open turn.
    TurnState current_;
    std::unordered_map<std::string, NodeId> turns_;
    std::vector<NodeId> turn_seq_;
    // Index into `turn_seq_` of the first turn the next compaction covers.
    std::size_t covered_ = 0;
    // The user message `begin_turn` inserted, waiting for its native text part: (message id, node, text).
    struct Binding {
        std::string message;
        NodeId node;
        std::string text;
    };
    std::optional<Binding> bind_;
    std::unordered_map<std::string, Msg> messages_;
    std::unordered_map<std::string, Part> parts_;
    // Native call id to (`<call>`, `<turn>`).
    std::unordered_map<std::string, std::pair<NodeId, NodeId>> calls_;
    std::map<NodeId, Pending> decisions_;
    std::vector<std::pair<NodeId, Todo>> todos_;
    std::unordered_map<std::string, Child> children_;
    std::optional<std::string> model_;
};

} // namespace loa::harness::opencode
