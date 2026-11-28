#include "loa/harness/acp/wire.hpp"

#include <array>
#include <format>
#include <limits>
#include <utility>

namespace loa::harness::acp::wire {

namespace {

using loa::runtime::json::Array;

std::string type_name(const Json& v) {
    if (v.is_null()) return "null";
    if (v.is_bool()) return std::format("boolean `{}`", *v.as_bool());
    if (auto i = v.as_i64()) return std::format("integer `{}`", *i);
    if (auto u = v.as_u64()) return std::format("integer `{}`", *u);
    if (auto f = v.as_f64()) return std::format("floating point `{}`", *f);
    if (v.is_string()) return std::format("string \"{}\"", *v.as_string());
    if (v.is_array()) return "sequence";
    return "map";
}

std::string invalid(const Json& v, std::string_view expected) {
    return std::format("invalid type: {}, expected {}", type_name(v), expected);
}

// The members of a serde struct: lookups with serde's error texts.
class Fields {
  public:
    static Decoded<Fields> of(const Json& v, std::string_view name) {
        if (!v.is_object()) return std::unexpected(invalid(v, std::format("struct {}", name)));
        return Fields(v);
    }

    // Absent, or `null` for optional fields.
    [[nodiscard]] const Json* get(std::string_view key) const { return v_->find(key); }

    Decoded<std::string> str(std::string_view key) const {
        const Json* item = get(key);
        if (item == nullptr) return std::unexpected(std::format("missing field `{}`", key));
        if (!item->as_string()) return std::unexpected(invalid(*item, "a string"));
        return *item->as_string();
    }
    Decoded<std::optional<std::string>> opt_str(std::string_view key) const {
        const Json* item = get(key);
        if (item == nullptr || item->is_null()) return std::nullopt;
        if (!item->as_string()) return std::unexpected(invalid(*item, "a string"));
        return *item->as_string();
    }
    // A `#[serde(default)] String`.
    Decoded<std::string> default_str(std::string_view key) const {
        if (get(key) == nullptr) return std::string();
        return str(key);
    }
    Decoded<bool> default_bool(std::string_view key) const {
        const Json* item = get(key);
        if (item == nullptr) return false;
        if (!item->as_bool()) return std::unexpected(invalid(*item, "a boolean"));
        return *item->as_bool();
    }
    template <typename T>
    Decoded<std::optional<T>> opt_uint(std::string_view key, std::string_view what) const {
        const Json* item = get(key);
        if (item == nullptr || item->is_null()) return std::nullopt;
        auto n = item->as_u64();
        if (!n) {
            if (item->as_i64()) return std::unexpected(std::format("invalid value: {}, expected {}", type_name(*item), what));
            return std::unexpected(invalid(*item, what));
        }
        if (*n > std::numeric_limits<T>::max())
            return std::unexpected(std::format("invalid value: integer `{}`, expected {}", *n, what));
        return static_cast<T>(*n);
    }
    // An optional array (absent or `null` is none).
    Decoded<const Array*> opt_array(std::string_view key) const {
        const Json* item = get(key);
        if (item == nullptr || item->is_null()) return nullptr;
        if (!item->as_array()) return std::unexpected(invalid(*item, "a sequence"));
        return item->as_array();
    }
    std::optional<Json> opt_any(std::string_view key) const {
        const Json* item = get(key);
        if (item == nullptr || item->is_null()) return std::nullopt;
        return *item;
    }

  private:
    explicit Fields(const Json& v) : v_(&v) {}
    const Json* v_;
};

template <typename E, std::size_t N>
Decoded<E> variant(const Json& v, const std::array<std::string_view, N>& names) {
    if (!v.as_string()) return std::unexpected(invalid(v, "a string"));
    for (std::size_t i = 0; i < N; ++i)
        if (*v.as_string() == names[i]) return static_cast<E>(i);
    std::string expected;
    for (std::size_t i = 0; i < N; ++i) {
        if (i > 0) expected += i + 1 == N ? " or " : ", ";
        expected += std::format("`{}`", names[i]);
    }
    return std::unexpected(std::format("unknown variant `{}`, expected {}", *v.as_string(), N == 1 ? expected : "one of " + expected));
}

constexpr std::array<std::string_view, 5> STOP_REASONS{"end_turn", "max_tokens", "max_turn_requests", "refusal", "cancelled"};
constexpr std::array<std::string_view, 4> TOOL_STATUSES{"pending", "in_progress", "completed", "failed"};
constexpr std::array<std::string_view, 3> PLAN_STATUSES{"pending", "in_progress", "completed"};
constexpr std::array<std::string_view, 4> OPTION_KINDS{"allow_once", "allow_always", "reject_once", "reject_always"};

#define LOA_TRY(var, expr)                                     \
    auto var##_r = (expr);                                     \
    if (!var##_r) return std::unexpected(std::move(var##_r).error()); \
    auto var = std::move(*var##_r)

Decoded<AgentCapabilities> decode_agent_capabilities(const Json* v) {
    AgentCapabilities out;
    if (v == nullptr) return out;
    LOA_TRY(f, Fields::of(*v, "AgentCapabilities"));
    LOA_TRY(load, f.default_bool("loadSession"));
    out.load_session = load;
    if (const Json* mcp = f.get("mcpCapabilities")) {
        LOA_TRY(m, Fields::of(*mcp, "McpCapabilities"));
        LOA_TRY(http, m.default_bool("http"));
        out.mcp_capabilities.http = http;
    }
    return out;
}

Decoded<std::optional<SessionModelState>> decode_models(const Json* v) {
    if (v == nullptr || v->is_null()) return std::nullopt;
    LOA_TRY(f, Fields::of(*v, "SessionModelState"));
    SessionModelState out;
    LOA_TRY(current, f.str("currentModelId"));
    out.current_model_id = std::move(current);
    LOA_TRY(models, f.opt_array("availableModels"));
    if (models != nullptr) {
        for (const auto& item : *models) {
            LOA_TRY(m, Fields::of(item, "ModelInfo"));
            LOA_TRY(id, m.str("modelId"));
            LOA_TRY(name, m.default_str("name"));
            out.available_models.push_back({std::move(id), std::move(name)});
        }
    }
    return out;
}

Decoded<std::optional<SessionModeState>> decode_modes(const Json* v) {
    if (v == nullptr || v->is_null()) return std::nullopt;
    LOA_TRY(f, Fields::of(*v, "SessionModeState"));
    SessionModeState out;
    LOA_TRY(current, f.str("currentModeId"));
    out.current_mode_id = std::move(current);
    LOA_TRY(modes, f.opt_array("availableModes"));
    if (modes != nullptr) {
        for (const auto& item : *modes) {
            LOA_TRY(m, Fields::of(item, "SessionModeInfo"));
            LOA_TRY(id, m.str("id"));
            out.available_modes.push_back({std::move(id)});
        }
    }
    return out;
}

} // namespace

std::string_view as_str(StopReason reason) noexcept { return STOP_REASONS[static_cast<std::size_t>(reason)]; }
std::string_view as_str(PermissionOptionKind kind) noexcept { return OPTION_KINDS[static_cast<std::size_t>(kind)]; }

Decoded<InitializeResponse> decode_initialize(const Json& v) {
    LOA_TRY(f, Fields::of(v, "InitializeResponse"));
    InitializeResponse out;
    const Json* version = f.get("protocolVersion");
    if (version == nullptr) return std::unexpected("missing field `protocolVersion`");
    LOA_TRY(parsed, f.opt_uint<std::uint16_t>("protocolVersion", "u16"));
    if (!parsed) return std::unexpected(invalid(*version, "u16"));
    out.protocol_version = *parsed;
    LOA_TRY(caps, decode_agent_capabilities(f.get("agentCapabilities")));
    out.agent_capabilities = caps;
    if (const Json* info = f.get("agentInfo"); info != nullptr && !info->is_null()) {
        LOA_TRY(i, Fields::of(*info, "Implementation"));
        LOA_TRY(name, i.str("name"));
        LOA_TRY(ver, i.opt_str("version"));
        out.agent_info = Implementation{std::move(name), std::move(ver)};
    }
    return out;
}

Decoded<NewSessionResponse> decode_new_session(const Json& v) {
    LOA_TRY(f, Fields::of(v, "NewSessionResponse"));
    NewSessionResponse out;
    LOA_TRY(id, f.str("sessionId"));
    out.session_id = std::move(id);
    LOA_TRY(modes, decode_modes(f.get("modes")));
    out.modes = std::move(modes);
    LOA_TRY(models, decode_models(f.get("models")));
    out.models = std::move(models);
    return out;
}

Decoded<LoadSessionResponse> decode_load_session(const Json& v) {
    LOA_TRY(f, Fields::of(v, "LoadSessionResponse"));
    LoadSessionResponse out;
    LOA_TRY(modes, decode_modes(f.get("modes")));
    out.modes = std::move(modes);
    LOA_TRY(models, decode_models(f.get("models")));
    out.models = std::move(models);
    return out;
}

Decoded<PromptResponse> decode_prompt_response(const Json& v) {
    LOA_TRY(f, Fields::of(v, "PromptResponse"));
    const Json* reason = f.get("stopReason");
    if (reason == nullptr) return std::unexpected("missing field `stopReason`");
    LOA_TRY(parsed, variant<StopReason>(*reason, STOP_REASONS));
    return PromptResponse{parsed};
}

Decoded<ToolCallFields> decode_tool_call(const Json& v) {
    LOA_TRY(f, Fields::of(v, "ToolCallFields"));
    ToolCallFields out;
    LOA_TRY(id, f.str("toolCallId"));
    out.tool_call_id = std::move(id);
    LOA_TRY(title, f.opt_str("title"));
    out.title = std::move(title);
    LOA_TRY(name, f.opt_str("name"));
    out.name = std::move(name);
    LOA_TRY(kind, f.opt_str("kind"));
    out.kind = std::move(kind);
    if (const Json* status = f.get("status"); status != nullptr && !status->is_null()) {
        LOA_TRY(parsed, variant<ToolCallStatus>(*status, TOOL_STATUSES));
        out.status = parsed;
    }
    LOA_TRY(content, f.opt_array("content"));
    if (content != nullptr) out.content = *content;
    out.raw_input = f.opt_any("rawInput");
    out.raw_output = f.opt_any("rawOutput");
    return out;
}

Decoded<Plan> decode_plan(const Json& v) {
    LOA_TRY(f, Fields::of(v, "Plan"));
    Plan out;
    const Json* entries = f.get("entries");
    if (entries == nullptr) return out;
    if (!entries->as_array()) return std::unexpected(invalid(*entries, "a sequence"));
    for (const auto& item : *entries->as_array()) {
        LOA_TRY(e, Fields::of(item, "PlanEntry"));
        LOA_TRY(content, e.str("content"));
        LOA_TRY(priority, e.opt_str("priority"));
        const Json* status = e.get("status");
        if (status == nullptr) return std::unexpected("missing field `status`");
        LOA_TRY(parsed, variant<PlanEntryStatus>(*status, PLAN_STATUSES));
        out.entries.push_back({std::move(content), std::move(priority), parsed});
    }
    return out;
}

Decoded<CurrentModeUpdate> decode_current_mode(const Json& v) {
    LOA_TRY(f, Fields::of(v, "CurrentModeUpdate"));
    LOA_TRY(id, f.str("currentModeId"));
    return CurrentModeUpdate{std::move(id)};
}

Decoded<SessionInfoUpdate> decode_session_info(const Json& v) {
    LOA_TRY(f, Fields::of(v, "SessionInfoUpdate"));
    LOA_TRY(title, f.opt_str("title"));
    return SessionInfoUpdate{std::move(title)};
}

Decoded<RequestPermissionRequest> decode_permission(const Json& v) {
    LOA_TRY(f, Fields::of(v, "RequestPermissionRequest"));
    const Json* call = f.get("toolCall");
    if (call == nullptr) return std::unexpected("missing field `toolCall`");
    LOA_TRY(fields, decode_tool_call(*call));
    const Json* options = f.get("options");
    if (options == nullptr) return std::unexpected("missing field `options`");
    if (!options->as_array()) return std::unexpected(invalid(*options, "a sequence"));
    RequestPermissionRequest out{std::move(fields), {}};
    for (const auto& item : *options->as_array()) {
        LOA_TRY(o, Fields::of(item, "PermissionOption"));
        LOA_TRY(id, o.str("optionId"));
        LOA_TRY(name, o.str("name"));
        const Json* kind = o.get("kind");
        if (kind == nullptr) return std::unexpected("missing field `kind`");
        LOA_TRY(parsed, variant<PermissionOptionKind>(*kind, OPTION_KINDS));
        out.options.push_back({std::move(id), std::move(name), parsed});
    }
    return out;
}

Decoded<ReadTextFileRequest> decode_read_text_file(const Json& v) {
    LOA_TRY(f, Fields::of(v, "ReadTextFileRequest"));
    LOA_TRY(path, f.str("path"));
    LOA_TRY(line, f.opt_uint<std::uint32_t>("line", "u32"));
    LOA_TRY(limit, f.opt_uint<std::uint32_t>("limit", "u32"));
    return ReadTextFileRequest{std::move(path), line, limit};
}

Decoded<WriteTextFileRequest> decode_write_text_file(const Json& v) {
    LOA_TRY(f, Fields::of(v, "WriteTextFileRequest"));
    LOA_TRY(path, f.str("path"));
    LOA_TRY(content, f.str("content"));
    return WriteTextFileRequest{std::move(path), std::move(content)};
}

Decoded<CreateTerminalRequest> decode_create_terminal(const Json& v) {
    LOA_TRY(f, Fields::of(v, "CreateTerminalRequest"));
    CreateTerminalRequest out;
    LOA_TRY(command, f.str("command"));
    out.command = std::move(command);
    LOA_TRY(args, f.opt_array("args"));
    if (args != nullptr) {
        out.args.emplace();
        for (const auto& item : *args) {
            if (!item.as_string()) return std::unexpected(invalid(item, "a string"));
            out.args->push_back(*item.as_string());
        }
    }
    LOA_TRY(env, f.opt_array("env"));
    if (env != nullptr) {
        out.env.emplace();
        for (const auto& item : *env) {
            LOA_TRY(e, Fields::of(item, "EnvVariable"));
            LOA_TRY(name, e.str("name"));
            LOA_TRY(value, e.str("value"));
            out.env->push_back({std::move(name), std::move(value)});
        }
    }
    LOA_TRY(cwd, f.opt_str("cwd"));
    out.cwd = std::move(cwd);
    LOA_TRY(limit, f.opt_uint<std::uint64_t>("outputByteLimit", "u64"));
    out.output_byte_limit = limit;
    return out;
}

Decoded<TerminalRef> decode_terminal_ref(const Json& v) {
    LOA_TRY(f, Fields::of(v, "TerminalRef"));
    LOA_TRY(id, f.str("terminalId"));
    return TerminalRef{std::move(id)};
}

#undef LOA_TRY

} // namespace loa::harness::acp::wire
