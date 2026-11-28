#include "loa/settings/models/rpc.hpp"

#include <array>
#include <algorithm>
#include <utility>

namespace loa::settings::models::rpc {

namespace json = runtime::json;

namespace {

loa::Error decode_error(std::string_view what, std::string_view detail) {
    return loa::Error(ErrorCode::parse, std::string(what) + ": " + std::string(detail));
}

std::expected<const json::Object*, loa::Error> object_of(const json::Value& v, std::string_view what) {
    const json::Object* o = v.as_object();
    if (o == nullptr) return std::unexpected(decode_error(what, "expected an object"));
    return o;
}

// Required string member.
std::expected<std::string, loa::Error> req_str(const json::Value& v, std::string_view key, std::string_view what) {
    const json::Value* m = v.find(key);
    if (m == nullptr) return std::unexpected(decode_error(what, "missing field `" + std::string(key) + "`"));
    const std::string* s = m->as_string();
    if (s == nullptr) return std::unexpected(decode_error(what, "field `" + std::string(key) + "` is not a string"));
    return *s;
}

std::expected<bool, loa::Error> req_bool(const json::Value& v, std::string_view key, std::string_view what) {
    const json::Value* m = v.find(key);
    if (m == nullptr) return std::unexpected(decode_error(what, "missing field `" + std::string(key) + "`"));
    const bool* b = m->as_bool();
    if (b == nullptr) return std::unexpected(decode_error(what, "field `" + std::string(key) + "` is not a boolean"));
    return *b;
}

std::expected<double, loa::Error> req_f64(const json::Value& v, std::string_view key, std::string_view what) {
    const json::Value* m = v.find(key);
    if (m == nullptr) return std::unexpected(decode_error(what, "missing field `" + std::string(key) + "`"));
    auto d = m->as_f64();
    if (!d) return std::unexpected(decode_error(what, "field `" + std::string(key) + "` is not a number"));
    return *d;
}

// `#[serde(default)] Option<String>`: absent or null -> nullopt.
std::expected<std::optional<std::string>, loa::Error> opt_str(const json::Value& v, std::string_view key,
                                                              std::string_view what) {
    const json::Value* m = v.find(key);
    if (m == nullptr || m->is_null()) return std::optional<std::string>{};
    const std::string* s = m->as_string();
    if (s == nullptr) return std::unexpected(decode_error(what, "field `" + std::string(key) + "` is not a string"));
    return std::optional<std::string>{*s};
}

std::expected<std::optional<std::uint64_t>, loa::Error> opt_u64(const json::Value& v, std::string_view key,
                                                                std::string_view what) {
    const json::Value* m = v.find(key);
    if (m == nullptr || m->is_null()) return std::optional<std::uint64_t>{};
    auto n = m->as_u64();
    if (!n) return std::unexpected(decode_error(what, "field `" + std::string(key) + "` is not an unsigned integer"));
    return std::optional<std::uint64_t>{*n};
}

// `#[serde(default)] Vec<String>`: absent -> empty (null is rejected, as serde does).
std::expected<std::vector<std::string>, loa::Error> default_strs(const json::Value& v, std::string_view key,
                                                                 std::string_view what) {
    std::vector<std::string> out;
    const json::Value* m = v.find(key);
    if (m == nullptr) return out;
    const json::Array* a = m->as_array();
    if (a == nullptr) return std::unexpected(decode_error(what, "field `" + std::string(key) + "` is not an array"));
    out.reserve(a->size());
    for (const auto& item : *a) {
        const std::string* s = item.as_string();
        if (s == nullptr) return std::unexpected(decode_error(what, "field `" + std::string(key) + "` holds a non-string"));
        out.push_back(*s);
    }
    return out;
}

json::Value opt_json(const std::optional<std::string>& s) { return s ? json::Value(*s) : json::Value(nullptr); }
json::Value opt_json(const std::optional<std::uint64_t>& n) { return n ? json::Value(*n) : json::Value(nullptr); }

json::Value strs_json(const std::vector<std::string>& v) {
    json::Array a;
    a.reserve(v.size());
    for (const auto& s : v) a.emplace_back(s);
    return a;
}

#define LOA_TRY(var, expr)                                                                                             \
    auto var##_r = (expr);                                                                                             \
    if (!var##_r) return std::unexpected(std::move(var##_r.error()));                                                  \
    auto var = std::move(*var##_r)

} // namespace

const std::string& response_id(const UiResponse& response) noexcept {
    return std::visit([](const auto& r) -> const std::string& { return r.id; }, response);
}

// --- LoginProvider ---

std::expected<LoginProvider, loa::Error> login_provider_from_json(const json::Value& v) {
    constexpr std::string_view what = "LoginProvider";
    LOA_TRY(obj, object_of(v, what));
    (void)obj;
    LOA_TRY(id, req_str(v, "id", what));
    LOA_TRY(name, req_str(v, "name", what));
    LOA_TRY(available, req_bool(v, "available", what));
    LOA_TRY(authenticated, req_bool(v, "authenticated", what));
    return LoginProvider{std::move(id), std::move(name), available, authenticated};
}

json::Value to_json(const LoginProvider& v) {
    return json::Object{{"id", v.id}, {"name", v.name}, {"available", v.available}, {"authenticated", v.authenticated}};
}

// --- ModelCost ---

std::expected<ModelCost, loa::Error> model_cost_from_json(const json::Value& v) {
    constexpr std::string_view what = "ModelCost";
    LOA_TRY(obj, object_of(v, what));
    (void)obj;
    LOA_TRY(input, req_f64(v, "input", what));
    LOA_TRY(output, req_f64(v, "output", what));
    LOA_TRY(cache_read, req_f64(v, "cacheRead", what));
    LOA_TRY(cache_write, req_f64(v, "cacheWrite", what));
    return ModelCost{input, output, cache_read, cache_write};
}

json::Value to_json(const ModelCost& v) {
    return json::Object{
        {"input", v.input}, {"output", v.output}, {"cacheRead", v.cache_read}, {"cacheWrite", v.cache_write}};
}

// --- ThinkingConfig ---

std::expected<ThinkingConfig, loa::Error> thinking_config_from_json(const json::Value& v) {
    constexpr std::string_view what = "ThinkingConfig";
    LOA_TRY(obj, object_of(v, what));
    (void)obj;
    LOA_TRY(mode, req_str(v, "mode", what));
    LOA_TRY(efforts, default_strs(v, "efforts", what));
    LOA_TRY(default_level, opt_str(v, "defaultLevel", what));
    return ThinkingConfig{std::move(mode), std::move(efforts), std::move(default_level)};
}

json::Value to_json(const ThinkingConfig& v) {
    return json::Object{{"mode", v.mode}, {"efforts", strs_json(v.efforts)}, {"defaultLevel", opt_json(v.default_level)}};
}

// --- ModelInfo ---

std::expected<ModelInfo, loa::Error> model_info_from_json(const json::Value& v) {
    constexpr std::string_view what = "ModelInfo";
    static constexpr std::array<std::string_view, 10> known{
        "id", "name", "provider", "api", "reasoning", "input", "cost", "contextWindow", "maxTokens", "thinking"};
    LOA_TRY(obj, object_of(v, what));
    ModelInfo m;
    LOA_TRY(id, req_str(v, "id", what));
    LOA_TRY(name, req_str(v, "name", what));
    LOA_TRY(provider, req_str(v, "provider", what));
    m.id = std::move(id);
    m.name = std::move(name);
    m.provider = std::move(provider);
    if (const json::Value* api = v.find("api")) {
        const std::string* s = api->as_string();
        if (s == nullptr) return std::unexpected(decode_error(what, "field `api` is not a string"));
        m.api = *s;
    }
    if (const json::Value* r = v.find("reasoning")) {
        const bool* b = r->as_bool();
        if (b == nullptr) return std::unexpected(decode_error(what, "field `reasoning` is not a boolean"));
        m.reasoning = *b;
    }
    LOA_TRY(input, default_strs(v, "input", what));
    m.input = std::move(input);
    if (const json::Value* c = v.find("cost")) {
        LOA_TRY(cost, model_cost_from_json(*c));
        m.cost = cost;
    }
    LOA_TRY(context_window, opt_u64(v, "contextWindow", what));
    LOA_TRY(max_tokens, opt_u64(v, "maxTokens", what));
    m.context_window = context_window;
    m.max_tokens = max_tokens;
    if (const json::Value* t = v.find("thinking"); t != nullptr && !t->is_null()) {
        LOA_TRY(thinking, thinking_config_from_json(*t));
        m.thinking = std::move(thinking);
    }
    for (const auto& [key, value] : *obj) {
        if (std::find(known.begin(), known.end(), key) == known.end()) m.extra.emplace_back(key, value);
    }
    return m;
}

json::Value to_json(const ModelInfo& v) {
    json::Object o{
        {"id", v.id},
        {"name", v.name},
        {"provider", v.provider},
        {"api", v.api},
        {"reasoning", v.reasoning},
        {"input", strs_json(v.input)},
        {"cost", to_json(v.cost)},
        {"contextWindow", opt_json(v.context_window)},
        {"maxTokens", opt_json(v.max_tokens)},
        {"thinking", v.thinking ? to_json(*v.thinking) : json::Value(nullptr)},
    };
    o.insert(o.end(), v.extra.begin(), v.extra.end());
    return o;
}

// --- LoginUiRequest ---

std::expected<LoginUiRequest, loa::Error> login_ui_request_from_json(const json::Value& v) {
    constexpr std::string_view what = "LoginUiRequest";
    LOA_TRY(obj, object_of(v, what));
    (void)obj;
    LOA_TRY(method, req_str(v, "method", what));
    LOA_TRY(id, req_str(v, "id", what));
    if (method == "open_url") {
        LOA_TRY(url, req_str(v, "url", what));
        LOA_TRY(launch_url, opt_str(v, "launchUrl", what));
        LOA_TRY(instructions, opt_str(v, "instructions", what));
        return OpenUrl{std::move(id), std::move(url), std::move(launch_url), std::move(instructions)};
    }
    if (method == "notify") {
        LOA_TRY(message, req_str(v, "message", what));
        LOA_TRY(notify_type, opt_str(v, "notifyType", what));
        return Notify{std::move(id), std::move(message), std::move(notify_type)};
    }
    if (method == "input") {
        LOA_TRY(title, req_str(v, "title", what));
        LOA_TRY(placeholder, opt_str(v, "placeholder", what));
        LOA_TRY(timeout, opt_u64(v, "timeout", what));
        return Input{std::move(id), std::move(title), std::move(placeholder), timeout};
    }
    if (method == "cancel") {
        LOA_TRY(target_id, req_str(v, "targetId", what));
        return Cancel{std::move(id), std::move(target_id)};
    }
    return std::unexpected(decode_error(what, "unknown variant `" + method + "`"));
}

json::Value to_json(const LoginUiRequest& v) {
    struct Visitor {
        json::Value operator()(const OpenUrl& r) const {
            return json::Object{{"method", "open_url"},
                                {"id", r.id},
                                {"url", r.url},
                                {"launchUrl", opt_json(r.launch_url)},
                                {"instructions", opt_json(r.instructions)}};
        }
        json::Value operator()(const Notify& r) const {
            return json::Object{
                {"method", "notify"}, {"id", r.id}, {"message", r.message}, {"notifyType", opt_json(r.notify_type)}};
        }
        json::Value operator()(const Input& r) const {
            return json::Object{{"method", "input"},
                                {"id", r.id},
                                {"title", r.title},
                                {"placeholder", opt_json(r.placeholder)},
                                {"timeout", opt_json(r.timeout)}};
        }
        json::Value operator()(const Cancel& r) const {
            return json::Object{{"method", "cancel"}, {"id", r.id}, {"targetId", r.target_id}};
        }
    };
    return std::visit(Visitor{}, v);
}

// --- UiResponse ---

std::expected<UiResponse, loa::Error> ui_response_from_json(const json::Value& v) {
    constexpr std::string_view what = "UiResponse";
    LOA_TRY(obj, object_of(v, what));
    (void)obj;
    if (auto id = req_str(v, "id", what)) {
        if (auto value = req_str(v, "value", what)) return UiValue{*id, std::move(*value)};
        if (auto cancelled = req_bool(v, "cancelled", what)) return UiCancelled{std::move(*id), *cancelled};
    }
    return std::unexpected(decode_error(what, "data did not match any variant of untagged enum UiResponse"));
}

json::Value to_json(const UiResponse& v) {
    struct Visitor {
        json::Value operator()(const UiValue& r) const { return json::Object{{"id", r.id}, {"value", r.value}}; }
        json::Value operator()(const UiCancelled& r) const {
            return json::Object{{"id", r.id}, {"cancelled", r.cancelled}};
        }
    };
    return std::visit(Visitor{}, v);
}

// --- LogoutAccount ---

std::expected<LogoutAccount, loa::Error> logout_account_from_json(const json::Value& v) {
    constexpr std::string_view what = "LogoutAccount";
    LOA_TRY(obj, object_of(v, what));
    (void)obj;
    const json::Value* cid = v.find("credentialId");
    if (cid == nullptr) return std::unexpected(decode_error(what, "missing field `credentialId`"));
    auto credential_id = cid->as_i64();
    if (!credential_id) return std::unexpected(decode_error(what, "field `credentialId` is not an integer"));
    LOA_TRY(provider, req_str(v, "provider", what));
    LOA_TRY(label, req_str(v, "label", what));
    LOA_TRY(detail, req_str(v, "detail", what));
    LOA_TRY(kind, req_str(v, "type", what));
    LOA_TRY(active, req_bool(v, "active", what));
    return LogoutAccount{*credential_id, std::move(provider), std::move(label), std::move(detail), std::move(kind), active};
}

json::Value to_json(const LogoutAccount& v) {
    return json::Object{{"credentialId", v.credential_id},
                        {"provider", v.provider},
                        {"label", v.label},
                        {"detail", v.detail},
                        {"type", v.kind},
                        {"active", v.active}};
}

#undef LOA_TRY

} // namespace loa::settings::models::rpc
