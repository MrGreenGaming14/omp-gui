#pragma once

// Element schema v1: kinds, attribute names, allowed parents and attribute
// types. `Dom::check` enforces it on every `insert` and `set`.
//
// Extension rules, so adapters never need a schema change to keep data:
// - any element accepts attributes whose name starts with `x-`, of any type
// - `<ext>` accepts any attribute and may sit under any element

#include <array>
#include <cstdint>
#include <expected>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>

#include "loa/core/value.hpp"
#include "loa/runtime/error.hpp"

namespace loa::core {

// Version of the element schema. `<session schema="…">` records it.
inline constexpr std::uint32_t SCHEMA_VERSION = 1;
// Reserved attribute name: a `set` on it replaces (or clears) the node text.
inline constexpr std::string_view TEXT = "#text";
// Prefix of attributes that bypass the schema.
inline constexpr std::string_view EXTENSION_PREFIX = "x-";

#define LOA_CORE_STR_ENUM(Name, ...)                                                                                  \
    template <>                                                                                                        \
    struct StrEnum<Name> {                                                                                             \
        static constexpr auto values = std::to_array<std::string_view>({__VA_ARGS__});                                                            \
    }

enum class Role : std::uint8_t { User, Assistant, System };
LOA_CORE_STR_ENUM(Role, "user", "assistant", "system");
enum class MessageKind : std::uint8_t { Text, Thinking };
LOA_CORE_STR_ENUM(MessageKind, "text", "thinking");
enum class MessageStatus : std::uint8_t { Streaming, Done, Error, Cancelled };
LOA_CORE_STR_ENUM(MessageStatus, "streaming", "done", "error", "cancelled");
enum class TurnStatus : std::uint8_t { Running, Waiting, Done, Error, Cancelled };
LOA_CORE_STR_ENUM(TurnStatus, "running", "waiting", "done", "error", "cancelled");
enum class CallStatus : std::uint8_t { Pending, Running, Done, Error, Cancelled };
LOA_CORE_STR_ENUM(CallStatus, "pending", "running", "done", "error", "cancelled");
enum class DecisionKind : std::uint8_t { Approval, Question, Plan, Form };
LOA_CORE_STR_ENUM(DecisionKind, "approval", "question", "plan", "form");
enum class DecisionStatus : std::uint8_t { Waiting, Answered, Expired, Cancelled };
LOA_CORE_STR_ENUM(DecisionStatus, "waiting", "answered", "expired", "cancelled");
enum class JobKind : std::uint8_t { Shell, Subagent };
LOA_CORE_STR_ENUM(JobKind, "shell", "subagent");
enum class JobStatus : std::uint8_t { Running, Done, Error, Cancelled };
LOA_CORE_STR_ENUM(JobStatus, "running", "done", "error", "cancelled");
enum class TodoState : std::uint8_t { Pending, InProgress, Done, Cancelled };
LOA_CORE_STR_ENUM(TodoState, "pending", "in_progress", "done", "cancelled");
enum class SurfaceMode : std::uint8_t { Pixel, Document };
LOA_CORE_STR_ENUM(SurfaceMode, "pixel", "document");
enum class SurfaceStatus : std::uint8_t { Open, Closed };
LOA_CORE_STR_ENUM(SurfaceStatus, "open", "closed");
enum class SessionMode : std::uint8_t { Normal, Plan };
LOA_CORE_STR_ENUM(SessionMode, "normal", "plan");

// Every element of schema v1. `as_str` is the tag name.
enum class ElementKind : std::uint8_t {
    Session, Turn, Message, Call, Input, Result, Usage, Decision, Job, Todo, Surface, Compaction, Digest, Ext,
};
LOA_CORE_STR_ENUM(ElementKind, "session", "turn", "message", "call", "input", "result", "usage",
                  "decision", "job", "todo", "surface", "compaction", "digest", "ext");

#undef LOA_CORE_STR_ENUM

// Attribute names of schema v1.
namespace attr {
inline constexpr std::string_view ID = "id", SCHEMA = "schema", HARNESS = "harness", PROJECT = "project",
                                  NODE = "node", CREATED = "created", TITLE = "title", CWD = "cwd", MODEL = "model",
                                  ROLE = "role", MODEL_ROLE = "model_role", EFFORT = "effort", MODE = "mode",
                                  PARENT_SESSION = "parent_session", FORKED_AT = "forked_at",
                                  NATIVE_ID = "native_id", EXCLUDED = "excluded", STATUS = "status",
                                  STARTED = "started", ENDED = "ended", KIND = "kind", TOOL = "tool",
                                  CALL_ID = "call_id", FORMAT = "format", IS_ERROR = "is_error",
                                  TRUNCATED = "truncated", INPUT_TOKENS = "input_tokens",
                                  OUTPUT_TOKENS = "output_tokens", CACHE_READ_TOKENS = "cache_read_tokens",
                                  CACHE_WRITE_TOKENS = "cache_write_tokens", COST = "cost",
                                  DURATION_MS = "duration_ms", OPTIONS = "options", MULTI = "multi",
                                  ANSWER = "answer", BY = "by", REASON = "reason", CALL = "call",
                                  DEFAULT = "default", TIMEOUT_MS = "timeout_ms", REQUEST_ID = "request_id",
                                  FIELDS = "fields", COMMAND = "command", SESSION = "session", AGENT = "agent",
                                  EXIT_CODE = "exit_code", STATE = "state", PHASE = "phase", URI = "uri",
                                  NAME = "name", WIDTH = "width", HEIGHT = "height", SERVER = "server",
                                  COVERS = "covers", TOKENS_BEFORE = "tokens_before", SOURCE_SEQ = "source_seq",
                                  STALE = "stale", DECISIONS = "decisions", FILES = "files",
                                  OPEN_ITEMS = "open_items", POINTERS = "pointers", TYPE = "type", RAW = "raw";
} // namespace attr

// Canonical `answer` values written on `<decision>` elements.
namespace answer {
// Approval: run once, allow for the session, or refuse (`reason` says why).
inline constexpr std::string_view ALLOW_ONCE = "allow_once", ALLOW_SESSION = "allow_session", DENY = "deny";
// Plan review: run as written, run the edited plan in the decision text, or
// refuse with feedback in `reason`.
inline constexpr std::string_view APPROVE = "approve", EDIT = "edit", REJECT = "reject";
} // namespace answer

// One attribute an element accepts.
struct AttrSpec {
    std::string_view name;
    ValueType ty;
    // Must be present on insert and can't be cleared.
    bool required;
    // Closed set of allowed strings. Empty means any value of `ty`.
    std::span<const std::string_view> values;
};

// The schema of one element.
struct ElementSpec {
    ElementKind kind;
    // Kinds this element may sit under. Empty means document root only.
    std::span<const ElementKind> parents;
    std::span<const AttrSpec> attrs;
    // Accepts attributes that are not listed in `attrs`.
    bool open;

    [[nodiscard]] const AttrSpec* attr(std::string_view name) const noexcept;
};

[[nodiscard]] const ElementSpec& spec(ElementKind kind) noexcept;

namespace schema_error {
struct Parent {
    ElementKind child;
    // `nullopt` is the document.
    std::optional<ElementKind> parent;
    friend bool operator==(const Parent&, const Parent&) = default;
};
struct UnknownAttr {
    ElementKind kind;
    std::string name;
    friend bool operator==(const UnknownAttr&, const UnknownAttr&) = default;
};
struct Type {
    ElementKind kind;
    std::string name;
    ValueType expected;
    ValueType got;
    friend bool operator==(const Type&, const Type&) = default;
};
struct EnumValue {
    ElementKind kind;
    std::string name;
    std::string value;
    friend bool operator==(const EnumValue&, const EnumValue&) = default;
};
struct Required {
    ElementKind kind;
    std::string_view name;
    friend bool operator==(const Required&, const Required&) = default;
};
struct TextType {
    friend bool operator==(const TextType&, const TextType&) = default;
};
} // namespace schema_error

using SchemaError = std::variant<schema_error::Parent, schema_error::UnknownAttr, schema_error::Type,
                                 schema_error::EnumValue, schema_error::Required, schema_error::TextType>;

// The Rust `Display` text.
[[nodiscard]] std::string to_string(const SchemaError& e);
[[nodiscard]] loa::Error to_error(const SchemaError& e);

// Checks that `kind` may be inserted under `parent` (`nullopt` is the document)
// with `attrs`.
[[nodiscard]] std::expected<void, SchemaError> validate_insert(std::optional<ElementKind> parent, ElementKind kind,
                                                              const Attrs& attrs);
// Checks a `set` of `name` on an element of `kind`. `nullptr` clears.
[[nodiscard]] std::expected<void, SchemaError> validate_set(ElementKind kind, std::string_view name, const Value* value);

} // namespace loa::core
