#pragma once

#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/runtime/json.hpp"

namespace loa::harness::opencode {

namespace json = loa::runtime::json;

struct Ref {
    std::string session, message, part, request;
};

struct Todo {
    std::string content, status, priority;
};

struct ToolRef {
    std::string call_id;
};

struct PermissionRequest {
    std::string id, session_id, permission;
    std::vector<std::string> patterns, always;
    json::Value metadata;
    std::optional<ToolRef> tool;
};

struct QuestionOption {
    std::string label;
};

struct QuestionInfo {
    std::string question, header;
    std::vector<QuestionOption> options;
    std::optional<bool> multiple, custom;
};

struct QuestionRequest {
    std::string id, session_id;
    std::vector<QuestionInfo> questions;
    std::optional<ToolRef> tool;
};

struct Known {
    enum class Kind {
        SessionCreated, SessionUpdated, SessionStatus, SessionIdle, SessionError,
        MessageUpdated, MessageRemoved, PartUpdated, PartDelta, PartRemoved,
        TodoUpdated, PermissionAsked, PermissionReplied, QuestionAsked,
        QuestionReplied, QuestionRejected
    };
    Kind kind;
    Ref at;
    json::Value info, status, error, part;
    std::string field, delta, reply;
    std::vector<Todo> todos;
    std::optional<PermissionRequest> permission;
    std::optional<QuestionRequest> question;
    std::vector<std::vector<std::string>> answers;
};

// Unknown and malformed-known events retain their original properties.
struct Event {
    std::string type;
    json::Value properties;
    std::optional<Known> known;
    [[nodiscard]] std::optional<std::string_view> session_id() const noexcept;
};

[[nodiscard]] std::optional<Event> parse_event(const json::Value& value);

// A todo list (`[{content, status, priority?}]`); `nullopt` when malformed.
[[nodiscard]] std::optional<std::vector<Todo>> parse_todos(const json::Value& value);

// Incremental SSE parser. Only blank-line-terminated data fields are dispatched.
class SseDecoder {
  public:
    void feed(std::string_view bytes, std::vector<std::string>& out);
  private:
    std::string pending_;
    std::optional<std::string> data_;
};

} // namespace loa::harness::opencode
