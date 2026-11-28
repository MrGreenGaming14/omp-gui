#include "loa/harness/omp/rpc/event.hpp"

#include <type_traits>
#include <utility>

#include "de.hpp"

namespace loa::harness::omp::rpc {
namespace {

bool assistant_event(const Json& data, AssistantEvent& out) {
    std::string type;
    if (!detail::string(data, "type", type)) return false;
    if (type == "text_start" || type == "thinking_start") {
        std::uint32_t index;
        if (!detail::uint32(data, "contentIndex", index)) return false;
        if (type == "text_start") out = assistant::TextStart{index};
        else out = assistant::ThinkingStart{index};
    } else if (type == "text_delta" || type == "thinking_delta" || type == "toolcall_delta") {
        std::uint32_t index;
        std::string delta;
        if (!detail::uint32(data, "contentIndex", index) || !detail::string(data, "delta", delta)) return false;
        if (type == "text_delta") out = assistant::TextDelta{index, std::move(delta)};
        else if (type == "thinking_delta") out = assistant::ThinkingDelta{index, std::move(delta)};
        else out = assistant::ToolcallDelta{index, std::move(delta)};
    } else if (type == "text_end" || type == "thinking_end") {
        std::uint32_t index;
        std::string content;
        if (!detail::uint32(data, "contentIndex", index) || !detail::string(data, "content", content)) return false;
        if (type == "text_end") out = assistant::TextEnd{index, std::move(content)};
        else out = assistant::ThinkingEnd{index, std::move(content)};
    } else if (type == "toolcall_start") {
        assistant::ToolcallStart step;
        if (!detail::uint32(data, "contentIndex", step.content_index) ||
            !detail::optional_json(data, "block", step.block) || !detail::optional_json(data, "partial", step.partial)) return false;
        out = std::move(step);
    } else if (type == "toolcall_end") {
        assistant::ToolcallEnd step;
        const auto* call = detail::field(data, "toolCall");
        if (!detail::uint32(data, "contentIndex", step.content_index) || !call) return false;
        step.tool_call = *call;
        out = std::move(step);
    } else if (type == "done" || type == "error") {
        std::string reason;
        if (!detail::string(data, "reason", reason)) return false;
        if (type == "done") out = assistant::Done{std::move(reason)};
        else out = assistant::Error{std::move(reason)};
    } else {
        out = assistant::Other{};
    }
    return true;
}

bool todo_items(const Json& data, std::vector<TodoItem>& out) {
    const auto* todos = detail::field(data, "todos");
    if (!todos) return true;
    if (!todos->as_array()) return false;
    out.reserve(todos->as_array()->size());
    for (const auto& item : *todos->as_array()) {
        TodoItem todo;
        std::string status;
        if (!detail::string(item, "content", todo.content) || !detail::string(item, "status", status)) return false;
        if (status == "pending") todo.status = TodoStatus::Pending;
        else if (status == "in_progress") todo.status = TodoStatus::InProgress;
        else if (status == "completed") todo.status = TodoStatus::Completed;
        else if (status == "abandoned") todo.status = TodoStatus::Abandoned;
        else return false;
        out.push_back(std::move(todo));
    }
    return true;
}

bool lifecycle(const Json& data, SubagentLifecycle& out) {
    std::string status;
    if (!detail::object(data) || !detail::string(data, "id", out.id) || !detail::string(data, "agent", out.agent) ||
        !detail::optional_string(data, "description", out.description) || !detail::string(data, "status", status) ||
        !detail::optional_string(data, "sessionFile", out.session_file)) return false;
    if (status == "started") out.status = SubagentStatus::Started;
    else if (status == "completed") out.status = SubagentStatus::Completed;
    else if (status == "failed") out.status = SubagentStatus::Failed;
    else if (status == "aborted") out.status = SubagentStatus::Aborted;
    else return false;
    return true;
}

bool progress(const Json& data, SubagentProgress& out) {
    return detail::object(data) && detail::optional_int64(data, "index", out.index) &&
           detail::string(data, "agent", out.agent) && detail::default_string(data, "task", out.task) &&
           detail::optional_string(data, "parentToolCallId", out.parent_tool_call_id) &&
           detail::default_json(data, "progress", out.progress) && detail::optional_string(data, "sessionFile", out.session_file);
}

bool prompt_error(const Json& data, PromptError& out) {
    return detail::object(data) && detail::string(data, "message", out.message) &&
           detail::optional_string(data, "provider", out.provider) && detail::optional_string(data, "model", out.model) &&
           detail::default_boolean(data, "retryable", out.retryable) && detail::optional_string(data, "code", out.code);
}

bool ask_questions(const Json& data, std::vector<AskQuestion>& out) {
    const auto* questions = detail::field(data, "questions");
    if (!questions || !questions->as_array()) return false;
    out.reserve(questions->as_array()->size());
    for (const auto& value : *questions->as_array()) {
        AskQuestion question;
        if (!detail::string(value, "id", question.id) || !detail::string(value, "question", question.question) ||
            !detail::optional_string(value, "header", question.header) || !detail::default_boolean(value, "multi", question.multi) ||
            !detail::optional_uint32(value, "recommended", question.recommended)) return false;
        if (const auto* options = detail::field(value, "options")) {
            if (!options->as_array()) return false;
            question.options.reserve(options->as_array()->size());
            for (const auto& option : *options->as_array()) {
                AskOption parsed;
                if (!detail::string(option, "label", parsed.label) || !detail::optional_string(option, "description", parsed.description)) return false;
                question.options.push_back(std::move(parsed));
            }
        }
        out.push_back(std::move(question));
    }
    return true;
}

} // namespace

template <> RpcResult<HostToolCall> decode_as<HostToolCall>(const Json& data) {
    HostToolCall out;
    if (!detail::object(data) || !detail::string(data, "id", out.id) ||
        !detail::string(data, "toolCallId", out.tool_call_id) || !detail::string(data, "toolName", out.tool_name) ||
        !detail::default_json(data, "arguments", out.arguments)) return std::unexpected(detail::json_error("host tool call"));
    return out;
}

Event event_from_json(Json frame) {
    std::string type;
    if (!detail::string(frame, "type", type)) return event::Unknown{std::move(frame)};
    if (type == "agent_start") return event::AgentStart{};
    if (type == "turn_start") return event::TurnStart{};
    if (type == "message_start" || type == "message_end") {
        Json message;
        std::optional<std::string> id;
        if (detail::default_json(frame, "message", message) && detail::optional_string(frame, "messageId", id)) {
            if (type == "message_start") return event::MessageStart{std::move(message), std::move(id)};
            return event::MessageEnd{std::move(message), std::move(id)};
        }
    } else if (type == "message_update") {
        event::MessageUpdate out;
        bool valid_role = true;
        if (const auto* message = detail::field(frame, "message")) {
            valid_role = detail::object(*message) && detail::default_string(*message, "role", out.role);
        }
        const auto* step = detail::field(frame, "assistantMessageEvent");
        if (valid_role && step && assistant_event(*step, out.event) && detail::optional_string(frame, "messageId", out.message_id))
            return out;
    } else if (type == "tool_execution_start") {
        event::ToolExecutionStart out;
        if (detail::string(frame, "toolCallId", out.tool_call_id) && detail::string(frame, "toolName", out.tool_name) &&
            detail::optional_json(frame, "args", out.args) && detail::optional_string(frame, "intent", out.intent)) return out;
    } else if (type == "tool_execution_update") {
        event::ToolExecutionUpdate out;
        if (detail::string(frame, "toolCallId", out.tool_call_id) && detail::string(frame, "toolName", out.tool_name) &&
            detail::optional_json(frame, "args", out.args) && detail::optional_json(frame, "partialResult", out.partial_result)) return out;
    } else if (type == "tool_execution_end") {
        event::ToolExecutionEnd out;
        if (detail::string(frame, "toolCallId", out.tool_call_id) && detail::string(frame, "toolName", out.tool_name) &&
            detail::optional_json(frame, "result", out.result) && detail::default_boolean(frame, "isError", out.is_error)) return out;
    } else if (type == "auto_compaction_end" || type == "manual_compaction_end") {
        event::CompactionEnd out;
        bool valid = true;
        if (const auto* result = detail::field(frame, "result"); result && !result->is_null()) {
            auto decoded = decode_as<CompactionResult>(*result);
            if (decoded) out.result = std::move(*decoded);
            else valid = false;
        }
        if (valid && detail::default_boolean(frame, "aborted", out.aborted) && detail::default_boolean(frame, "skipped", out.skipped) &&
            detail::optional_string(frame, "errorMessage", out.error_message)) return out;
    } else if (type == "thinking_level_changed") {
        event::ThinkingLevelChanged out;
        if (detail::optional_string(frame, "thinkingLevel", out.thinking_level)) return out;
    } else if (type == "todo_reminder") {
        event::TodoReminder out;
        if (todo_items(frame, out.todos)) return out;
    } else if (type == "todo_auto_clear") return event::TodoAutoClear{};
    else if (type == "notice") {
        event::Notice out;
        if (detail::default_string(frame, "level", out.level) && detail::default_string(frame, "message", out.message) &&
            detail::optional_string(frame, "source", out.source)) return out;
    } else if (type == "prompt_result") {
        event::PromptResult out;
        std::string status;
        if (detail::optional_string(frame, "id", out.id) && detail::default_boolean(frame, "agentInvoked", out.agent_invoked) &&
            detail::string(frame, "status", status) && detail::default_boolean(frame, "sessionSettled", out.session_settled)) {
            bool valid = true;
            if (status == "completed") out.status = PromptStatus::Completed;
            else if (status == "aborted") out.status = PromptStatus::Aborted;
            else if (status == "error") out.status = PromptStatus::Error;
            else valid = false;
            if (const auto* error = detail::field(frame, "error"); error && !error->is_null()) {
                PromptError parsed;
                if (prompt_error(*error, parsed)) out.error = std::move(parsed);
                else valid = false;
            }
            if (valid) return out;
        }
    } else if (type == "session_settled") return event::SessionSettled{};
    else if (type == "subagent_lifecycle") {
        SubagentLifecycle out;
        const auto* payload = detail::field(frame, "payload");
        if (payload && lifecycle(*payload, out)) return out;
    } else if (type == "subagent_progress") {
        SubagentProgress out;
        const auto* payload = detail::field(frame, "payload");
        if (payload && progress(*payload, out)) return out;
    } else if (type == "config_update") {
        event::ConfigUpdate out;
        bool valid = true;
        if (const auto* model = detail::field(frame, "model"); model && !model->is_null()) {
            auto decoded = decode_as<ModelInfo>(*model);
            if (decoded) out.model = std::move(*decoded);
            else valid = false;
        }
        if (valid && detail::optional_string(frame, "thinkingLevel", out.thinking_level)) return out;
    } else if (type == "session_info_update") {
        event::SessionInfoUpdate out;
        if (detail::optional_string(frame, "title", out.title) && detail::default_string(frame, "sessionId", out.session_id)) return out;
    }
    return event::Unknown{std::move(frame)};
}

template <> RpcResult<UiRequest> decode_as<UiRequest>(const Json& data) {
    std::string method;
    if (!detail::object(data) || !detail::string(data, "method", method)) return std::unexpected(detail::json_error("UI request"));
    std::string id;
    if (!detail::string(data, "id", id)) return std::unexpected(detail::json_error("UI request"));
    if (method == "select") {
        ui::Select out;
        out.id = std::move(id);
        if (detail::string(data, "title", out.title) && detail::string_array(data, "options", out.options) &&
            detail::optional_uint64(data, "timeout", out.timeout)) return out;
    } else if (method == "confirm") {
        ui::Confirm out;
        out.id = std::move(id);
        if (detail::string(data, "title", out.title) && detail::default_string(data, "message", out.message) &&
            detail::optional_uint64(data, "timeout", out.timeout)) return out;
    } else if (method == "input") {
        ui::Input out;
        out.id = std::move(id);
        if (detail::string(data, "title", out.title) && detail::optional_string(data, "placeholder", out.placeholder) &&
            detail::optional_uint64(data, "timeout", out.timeout) && detail::default_boolean(data, "secret", out.secret)) return out;
    } else if (method == "editor") {
        ui::Editor out;
        out.id = std::move(id);
        if (detail::string(data, "title", out.title) && detail::optional_string(data, "prefill", out.prefill)) return out;
    } else if (method == "ask") {
        ui::Ask out;
        out.id = std::move(id);
        if (ask_questions(data, out.questions) && detail::optional_uint64(data, "timeout", out.timeout)) return out;
    } else if (method == "cancel") {
        ui::Cancel out;
        out.id = std::move(id);
        if (detail::string(data, "targetId", out.target_id)) return out;
    } else if (method == "notify") {
        ui::Notify out;
        out.id = std::move(id);
        if (detail::string(data, "message", out.message) && detail::optional_string(data, "notifyType", out.notify_type)) return out;
    } else if (method == "setTitle") {
        ui::SetTitle out;
        out.id = std::move(id);
        if (detail::string(data, "title", out.title)) return out;
    } else if (method == "open_url") {
        ui::OpenUrl out;
        out.id = std::move(id);
        if (detail::string(data, "url", out.url) && detail::optional_string(data, "launchUrl", out.launch_url) &&
            detail::optional_string(data, "instructions", out.instructions)) return out;
    }
    return std::unexpected(detail::json_error("UI request"));
}

const std::string& ui_id(const UiRequest& request) {
    return std::visit([](const auto& value) -> const std::string& { return value.id; }, request);
}

bool is_dialog(const UiRequest& request) {
    return std::visit([](const auto& value) {
        using T = std::decay_t<decltype(value)>;
        return std::is_same_v<T, ui::Select> || std::is_same_v<T, ui::Confirm> ||
               std::is_same_v<T, ui::Input> || std::is_same_v<T, ui::Editor> || std::is_same_v<T, ui::Ask>;
    }, request);
}

Frame frame_from_json(Json frame) {
    if (const auto* type = detail::field(frame, "type"); type && type->as_string() && *type->as_string() == "extension_ui_request") {
        auto request = decode_as<UiRequest>(frame);
        if (request) return std::move(*request);
        return Event{event::Unknown{std::move(frame)}};
    }
    return event_from_json(std::move(frame));
}

} // namespace loa::harness::omp::rpc
