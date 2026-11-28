#pragma once

// Frames omp writes to stdout: session events, UI requests and host tool calls.

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/harness/omp/rpc/command.hpp"

namespace loa::harness::omp::rpc {

struct HostToolCall {
    std::string id;
    std::string tool_call_id;
    std::string tool_name;
    Json arguments;
    friend bool operator==(const HostToolCall&, const HostToolCall&) = default;
};

template <> RpcResult<HostToolCall> decode_as<HostToolCall>(const Json& data);

// A streaming step of an assistant message (`assistantMessageEvent`).
namespace assistant {
struct TextStart { std::uint32_t content_index = 0; friend bool operator==(const TextStart&, const TextStart&) = default; };
struct TextDelta {
    std::uint32_t content_index = 0;
    std::string delta;
    friend bool operator==(const TextDelta&, const TextDelta&) = default;
};
struct TextEnd {
    std::uint32_t content_index = 0;
    std::string content;
    friend bool operator==(const TextEnd&, const TextEnd&) = default;
};
struct ThinkingStart { std::uint32_t content_index = 0; friend bool operator==(const ThinkingStart&, const ThinkingStart&) = default; };
struct ThinkingDelta {
    std::uint32_t content_index = 0;
    std::string delta;
    friend bool operator==(const ThinkingDelta&, const ThinkingDelta&) = default;
};
struct ThinkingEnd {
    std::uint32_t content_index = 0;
    std::string content;
    friend bool operator==(const ThinkingEnd&, const ThinkingEnd&) = default;
};
struct ToolcallStart {
    std::uint32_t content_index = 0;
    // The tool call shell (id, name) in `delta` projection mode.
    std::optional<Json> block;
    // The accumulated message in `full` mode, which also carries the shell.
    std::optional<Json> partial;
    friend bool operator==(const ToolcallStart&, const ToolcallStart&) = default;
};
struct ToolcallDelta {
    std::uint32_t content_index = 0;
    std::string delta;
    friend bool operator==(const ToolcallDelta&, const ToolcallDelta&) = default;
};
struct ToolcallEnd {
    std::uint32_t content_index = 0;
    Json tool_call;
    friend bool operator==(const ToolcallEnd&, const ToolcallEnd&) = default;
};
struct Done { std::string reason; friend bool operator==(const Done&, const Done&) = default; };
struct Error { std::string reason; friend bool operator==(const Error&, const Error&) = default; };
// Any other step type.
struct Other { friend bool operator==(const Other&, const Other&) = default; };
} // namespace assistant

using AssistantEvent = std::variant<assistant::TextStart, assistant::TextDelta, assistant::TextEnd,
                                    assistant::ThinkingStart, assistant::ThinkingDelta, assistant::ThinkingEnd,
                                    assistant::ToolcallStart, assistant::ToolcallDelta, assistant::ToolcallEnd,
                                    assistant::Done, assistant::Error, assistant::Other>;

enum class PromptStatus { Completed, Aborted, Error };

struct PromptError {
    std::string message;
    std::optional<std::string> provider;
    std::optional<std::string> model;
    bool retryable = false;
    std::optional<std::string> code;
    friend bool operator==(const PromptError&, const PromptError&) = default;
};

enum class SubagentStatus { Started, Completed, Failed, Aborted };

struct SubagentLifecycle {
    std::string id;
    std::string agent;
    std::optional<std::string> description;
    SubagentStatus status = SubagentStatus::Started;
    std::optional<std::string> session_file;
    friend bool operator==(const SubagentLifecycle&, const SubagentLifecycle&) = default;
};

struct SubagentProgress {
    std::optional<std::int64_t> index;
    std::string agent;
    std::string task;
    std::optional<std::string> parent_tool_call_id;
    Json progress;
    std::optional<std::string> session_file;
    friend bool operator==(const SubagentProgress&, const SubagentProgress&) = default;
};

// Session events and notifications, tagged by their `type`. `SubagentLifecycle` and
// `SubagentProgress` (the `payload` of their frames) are alternatives of `Event` as well.
namespace event {
struct AgentStart { friend bool operator==(const AgentStart&, const AgentStart&) = default; };
struct TurnStart { friend bool operator==(const TurnStart&, const TurnStart&) = default; };
struct MessageStart {
    Json message;
    std::optional<std::string> message_id;
    friend bool operator==(const MessageStart&, const MessageStart&) = default;
};
struct MessageUpdate {
    // The `role` of the accumulated `message`.
    std::string role;
    // `assistantMessageEvent`.
    AssistantEvent event;
    std::optional<std::string> message_id;
    friend bool operator==(const MessageUpdate&, const MessageUpdate&) = default;
};
struct MessageEnd {
    Json message;
    std::optional<std::string> message_id;
    friend bool operator==(const MessageEnd&, const MessageEnd&) = default;
};
struct ToolExecutionStart {
    std::string tool_call_id;
    std::string tool_name;
    std::optional<Json> args;
    std::optional<std::string> intent;
    friend bool operator==(const ToolExecutionStart&, const ToolExecutionStart&) = default;
};
struct ToolExecutionUpdate {
    std::string tool_call_id;
    std::string tool_name;
    std::optional<Json> args;
    std::optional<Json> partial_result;
    friend bool operator==(const ToolExecutionUpdate&, const ToolExecutionUpdate&) = default;
};
struct ToolExecutionEnd {
    std::string tool_call_id;
    std::string tool_name;
    std::optional<Json> result;
    bool is_error = false;
    friend bool operator==(const ToolExecutionEnd&, const ToolExecutionEnd&) = default;
};
// `auto_compaction_end` or `manual_compaction_end`.
struct CompactionEnd {
    std::optional<CompactionResult> result;
    bool aborted = false;
    bool skipped = false;
    std::optional<std::string> error_message;
    friend bool operator==(const CompactionEnd&, const CompactionEnd&) = default;
};
struct ThinkingLevelChanged {
    std::optional<std::string> thinking_level;
    friend bool operator==(const ThinkingLevelChanged&, const ThinkingLevelChanged&) = default;
};
struct TodoReminder { std::vector<TodoItem> todos; friend bool operator==(const TodoReminder&, const TodoReminder&) = default; };
struct TodoAutoClear { friend bool operator==(const TodoAutoClear&, const TodoAutoClear&) = default; };
struct Notice {
    std::string level;
    std::string message;
    std::optional<std::string> source;
    friend bool operator==(const Notice&, const Notice&) = default;
};
struct PromptResult {
    std::optional<std::string> id;
    bool agent_invoked = false;
    PromptStatus status = PromptStatus::Completed;
    std::optional<PromptError> error;
    bool session_settled = false;
    friend bool operator==(const PromptResult&, const PromptResult&) = default;
};
struct SessionSettled { friend bool operator==(const SessionSettled&, const SessionSettled&) = default; };
struct ConfigUpdate {
    std::optional<ModelInfo> model;
    std::optional<std::string> thinking_level;
    friend bool operator==(const ConfigUpdate&, const ConfigUpdate&) = default;
};
struct SessionInfoUpdate {
    std::optional<std::string> title;
    std::string session_id;
    friend bool operator==(const SessionInfoUpdate&, const SessionInfoUpdate&) = default;
};
// A frame this client doesn't model, or whose shape doesn't match the model (so a newer omp
// never stops the client), raw.
struct Unknown { Json frame; friend bool operator==(const Unknown&, const Unknown&) = default; };
} // namespace event

using Event = std::variant<event::AgentStart, event::TurnStart, event::MessageStart, event::MessageUpdate,
                           event::MessageEnd, event::ToolExecutionStart, event::ToolExecutionUpdate,
                           event::ToolExecutionEnd, event::CompactionEnd, event::ThinkingLevelChanged,
                           event::TodoReminder, event::TodoAutoClear, event::Notice, event::PromptResult,
                           event::SessionSettled, SubagentLifecycle, SubagentProgress, event::ConfigUpdate,
                           event::SessionInfoUpdate, event::Unknown>;

// Decodes one event frame; see `event::Unknown`.
[[nodiscard]] Event event_from_json(Json frame);

// One question of an `ask` UI request.
struct AskOption {
    std::string label;
    std::optional<std::string> description;
    friend bool operator==(const AskOption&, const AskOption&) = default;
};

struct AskQuestion {
    std::string id;
    std::string question;
    std::optional<std::string> header;
    std::vector<AskOption> options;
    bool multi = false;
    std::optional<std::uint32_t> recommended;
    friend bool operator==(const AskQuestion&, const AskQuestion&) = default;
};

// An `extension_ui_request`, tagged by `method`. Dialogs (`select`, `confirm`, `input`,
// `editor`, `ask`) wait for a `UiResponse`; the rest are fire and forget.
namespace ui {
struct Select {
    std::string id;
    std::string title;
    std::vector<std::string> options;
    std::optional<std::uint64_t> timeout;
    friend bool operator==(const Select&, const Select&) = default;
};
struct Confirm {
    std::string id;
    std::string title;
    std::string message;
    std::optional<std::uint64_t> timeout;
    friend bool operator==(const Confirm&, const Confirm&) = default;
};
struct Input {
    std::string id;
    std::string title;
    std::optional<std::string> placeholder;
    std::optional<std::uint64_t> timeout;
    bool secret = false;
    friend bool operator==(const Input&, const Input&) = default;
};
struct Editor {
    std::string id;
    std::string title;
    std::optional<std::string> prefill;
    friend bool operator==(const Editor&, const Editor&) = default;
};
struct Ask {
    std::string id;
    std::vector<AskQuestion> questions;
    std::optional<std::uint64_t> timeout;
    friend bool operator==(const Ask&, const Ask&) = default;
};
// omp closed the dialog `target_id` itself (timeout or abort).
struct Cancel {
    std::string id;
    std::string target_id;
    friend bool operator==(const Cancel&, const Cancel&) = default;
};
struct Notify {
    std::string id;
    std::string message;
    std::optional<std::string> notify_type;
    friend bool operator==(const Notify&, const Notify&) = default;
};
struct SetTitle {
    std::string id;
    std::string title;
    friend bool operator==(const SetTitle&, const SetTitle&) = default;
};
struct OpenUrl {
    std::string id;
    std::string url;
    std::optional<std::string> launch_url;
    std::optional<std::string> instructions;
    friend bool operator==(const OpenUrl&, const OpenUrl&) = default;
};
} // namespace ui

using UiRequest = std::variant<ui::Select, ui::Confirm, ui::Input, ui::Editor, ui::Ask, ui::Cancel, ui::Notify,
                               ui::SetTitle, ui::OpenUrl>;

[[nodiscard]] const std::string& ui_id(const UiRequest& request);
// Whether omp waits for an answer.
[[nodiscard]] bool is_dialog(const UiRequest& request);

template <> RpcResult<UiRequest> decode_as<UiRequest>(const Json& data);

// An event or UI request frame.
using Frame = std::variant<Event, UiRequest>;

// A UI request this client doesn't model (status lines, widgets, …) stays an `event::Unknown`.
[[nodiscard]] Frame frame_from_json(Json frame);

} // namespace loa::harness::omp::rpc
