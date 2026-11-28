#include "loa/core/schema.hpp"

#include <algorithm>
#include <format>

namespace loa::core {

namespace {

using enum ValueType;
using K = ElementKind;

template <class E>
constexpr std::span<const std::string_view> values_of() {
    return StrEnum<E>::values;
}

constexpr AttrSpec req(std::string_view name, ValueType ty) { return {name, ty, true, {}}; }
constexpr AttrSpec opt(std::string_view name, ValueType ty) { return {name, ty, false, {}}; }
template <class E>
constexpr AttrSpec req_enum(std::string_view name) { return {name, Str, true, values_of<E>()}; }
template <class E>
constexpr AttrSpec opt_enum(std::string_view name) { return {name, Str, false, values_of<E>()}; }

using namespace attr;

// `node` is the owner's public key; `created` is unix ms; `parent_session`
// and `forked_at` record fork lineage; `native_id` is the harness's own id.
constexpr AttrSpec session_attrs[] = {
    req(ID, Str), req(HARNESS, Str), req(NODE, Str), req(CREATED, Int), opt(SCHEMA, Int), opt(PROJECT, Str),
    opt(TITLE, Str), opt(CWD, Str), opt(MODEL, Str), opt(MODEL_ROLE, Str), opt(EFFORT, Str),
    opt_enum<SessionMode>(MODE), opt(PARENT_SESSION, Str), opt(FORKED_AT, Int), opt(NATIVE_ID, Str),
    opt(EXCLUDED, Bool),
};
constexpr AttrSpec turn_attrs[] = {opt(ID, Str), opt_enum<TurnStatus>(STATUS), opt(STARTED, Int), opt(ENDED, Int)};
// An absent `kind` means "text".
constexpr AttrSpec message_attrs[] = {
    req_enum<Role>(ROLE), opt_enum<MessageKind>(KIND), opt(ID, Str), opt(MODEL, Str),
    opt_enum<MessageStatus>(STATUS), opt(BY, Str),
};
constexpr AttrSpec call_attrs[] = {
    req(TOOL, Str), opt_enum<CallStatus>(STATUS), opt(CALL_ID, Str), opt(TITLE, Str), opt(STARTED, Int),
    opt(ENDED, Int),
};
// Text holds the arguments; `format` is "json" unless stated.
constexpr AttrSpec input_attrs[] = {opt(FORMAT, Str)};
constexpr AttrSpec result_attrs[] = {opt(FORMAT, Str), opt(IS_ERROR, Bool), opt(TRUNCATED, Bool)};
constexpr AttrSpec usage_attrs[] = {
    opt(INPUT_TOKENS, Int), opt(OUTPUT_TOKENS, Int), opt(CACHE_READ_TOKENS, Int), opt(CACHE_WRITE_TOKENS, Int),
    opt(COST, Float), opt(DURATION_MS, Int), opt(MODEL, Str),
};
// Text holds the body. `answer` holds picked options, typed text or an
// `answer` value; `call` is the node id of the approved `<call>`; `default`
// applies when `timeout_ms` passes; `fields` is a JSON Schema.
constexpr AttrSpec decision_attrs[] = {
    req_enum<DecisionKind>(KIND), opt_enum<DecisionStatus>(STATUS), opt(TITLE, Str), opt(OPTIONS, List),
    opt(MULTI, Bool), opt(ANSWER, List), opt(BY, Str), opt(REASON, Str), opt(CALL, Int), opt(DEFAULT, List),
    opt(TIMEOUT_MS, Int), opt(REQUEST_ID, Str), opt(FIELDS, Str), opt(CREATED, Int), opt(ENDED, Int),
};
// Text holds output so far; `session` links a subagent's child session.
constexpr AttrSpec job_attrs[] = {
    req_enum<JobKind>(KIND), opt_enum<JobStatus>(STATUS), opt(ID, Str), opt(TITLE, Str), opt(COMMAND, Str),
    opt(SESSION, Str), opt(AGENT, Str), opt(EXIT_CODE, Int), opt(CALL, Int), opt(STARTED, Int), opt(ENDED, Int),
};
constexpr AttrSpec todo_attrs[] = {opt_enum<TodoState>(STATE), opt(ID, Str), opt(PHASE, Str)};
constexpr AttrSpec surface_attrs[] = {
    req(URI, Str), req_enum<SurfaceMode>(MODE), opt(NAME, Str), opt(WIDTH, Int), opt(HEIGHT, Int),
    opt(SERVER, Str), opt_enum<SurfaceStatus>(STATUS),
};
// Text holds the summary; `covers` lists the replaced turns' node ids.
constexpr AttrSpec compaction_attrs[] = {opt(COVERS, List), opt(TOKENS_BEFORE, Int), opt(MODEL, Str), opt(CREATED, Int)};
// `source_seq` is the patch it was built from; `pointers` are turn node ids.
constexpr AttrSpec digest_attrs[] = {
    req(SOURCE_SEQ, Int), opt(STALE, Bool), opt(DECISIONS, List), opt(FILES, List), opt(OPEN_ITEMS, List),
    opt(POINTERS, List), opt(MODEL, Str), opt(CREATED, Int),
};
// `raw` or the text keeps the harness's own data so round trips are lossless.
constexpr AttrSpec ext_attrs[] = {req(HARNESS, Str), opt(TYPE, Str)};

constexpr ElementKind under_session[] = {K::Session};
constexpr ElementKind under_turn[] = {K::Turn};
constexpr ElementKind under_call[] = {K::Call};
constexpr ElementKind under_usage[] = {K::Call, K::Message, K::Turn, K::Job};
constexpr ElementKind under_turn_call_session[] = {K::Turn, K::Call, K::Session};
constexpr ElementKind under_turn_session[] = {K::Turn, K::Session};
constexpr auto under_any = all<ElementKind>();

constexpr ElementSpec specs[] = {
    {K::Session, {}, session_attrs, false},
    {K::Turn, under_session, turn_attrs, false},
    {K::Message, under_turn, message_attrs, false},
    {K::Call, under_turn, call_attrs, false},
    {K::Input, under_call, input_attrs, false},
    {K::Result, under_call, result_attrs, false},
    {K::Usage, under_usage, usage_attrs, false},
    {K::Decision, under_turn_call_session, decision_attrs, false},
    {K::Job, under_turn_call_session, job_attrs, false},
    {K::Todo, under_turn_session, todo_attrs, false},
    {K::Surface, under_turn_call_session, surface_attrs, false},
    {K::Compaction, under_session, compaction_attrs, false},
    {K::Digest, under_session, digest_attrs, false},
    {K::Ext, under_any, ext_attrs, true},
};
static_assert(std::size(specs) == StrEnum<ElementKind>::values.size());

bool unlisted_ok(const ElementSpec& spec, std::string_view name) {
    return spec.open || name.starts_with(EXTENSION_PREFIX);
}

std::expected<void, SchemaError> validate_attr(const ElementSpec& spec, std::string_view name, const Value& value) {
    const ElementKind kind = spec.kind;
    const AttrSpec* a = spec.attr(name);
    if (!a) {
        if (unlisted_ok(spec, name)) return {};
        return std::unexpected(schema_error::UnknownAttr{kind, std::string(name)});
    }
    const ValueType got = value.type();
    if (got != a->ty) return std::unexpected(schema_error::Type{kind, std::string(name), a->ty, got});
    if (auto text = value.as_str(); text && !a->values.empty() && std::ranges::find(a->values, *text) == a->values.end())
        return std::unexpected(schema_error::EnumValue{kind, std::string(name), std::string(*text)});
    return {};
}

} // namespace

const AttrSpec* ElementSpec::attr(std::string_view name) const noexcept {
    auto it = std::ranges::find(attrs, name, &AttrSpec::name);
    return it == attrs.end() ? nullptr : &*it;
}

const ElementSpec& spec(ElementKind kind) noexcept { return specs[static_cast<std::size_t>(kind)]; }

std::string to_string(const SchemaError& e) {
    using namespace schema_error;
    return std::visit(
        [](const auto& v) -> std::string {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, Parent>) {
                std::string parent = v.parent ? std::format("<{}>", as_str(*v.parent)) : "the document";
                return std::format("<{}> can't sit under {}", as_str(v.child), parent);
            } else if constexpr (std::is_same_v<T, UnknownAttr>) {
                return std::format("<{}> has no attribute `{}`", as_str(v.kind), v.name);
            } else if constexpr (std::is_same_v<T, Type>) {
                return std::format("<{} {}> expects {}, got {}", as_str(v.kind), v.name, debug_name(v.expected),
                                   debug_name(v.got));
            } else if constexpr (std::is_same_v<T, EnumValue>) {
                return std::format("<{} {}> doesn't allow `{}`", as_str(v.kind), v.name, v.value);
            } else if constexpr (std::is_same_v<T, Required>) {
                return std::format("<{}> requires attribute `{}`", as_str(v.kind), v.name);
            } else {
                return std::format("`{}` takes a string", TEXT);
            }
        },
        e);
}

loa::Error to_error(const SchemaError& e) { return {ErrorCode::invalid_argument, to_string(e)}; }

std::expected<void, SchemaError> validate_insert(std::optional<ElementKind> parent, ElementKind kind,
                                                 const Attrs& attrs) {
    const ElementSpec& s = spec(kind);
    const bool parent_ok = parent ? std::ranges::find(s.parents, *parent) != s.parents.end() : s.parents.empty();
    if (!parent_ok) return std::unexpected(schema_error::Parent{kind, parent});
    for (const auto& [name, value] : attrs)
        if (auto ok = validate_attr(s, name, value); !ok) return ok;
    for (const AttrSpec& a : s.attrs)
        if (a.required && !attrs.contains(a.name)) return std::unexpected(schema_error::Required{kind, a.name});
    return {};
}

std::expected<void, SchemaError> validate_set(ElementKind kind, std::string_view name, const Value* value) {
    const ElementSpec& s = spec(kind);
    if (name == TEXT) {
        if (!value || value->as_str()) return {};
        return std::unexpected(schema_error::TextType{});
    }
    if (value) return validate_attr(s, name, *value);
    if (const AttrSpec* a = s.attr(name)) {
        if (a->required) return std::unexpected(schema_error::Required{kind, a->name});
        return {};
    }
    if (unlisted_ok(s, name)) return {};
    return std::unexpected(schema_error::UnknownAttr{kind, std::string(name)});
}

} // namespace loa::core
