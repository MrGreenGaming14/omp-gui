#include "loa/settings/models/models_yml.hpp"

#include <algorithm>
#include <array>
#include <charconv>
#include <cmath>
#include <limits>

#include <asio.hpp>

#include "text_util.hpp"

namespace loa::settings::models::models_yml {

using yaml::Yaml;
using yaml::YamlDocument;
using yaml::ymap;
using yaml::ystr;

namespace {

constexpr std::array<std::string_view, 3> AUTH_MODES = {"apiKey", "none", "oauth"};
constexpr std::array<std::string_view, 2> MODALITIES = {"text", "image"};
constexpr std::array<std::string_view, 10> OVERRIDE_KEYS = {
    "name",  "contextWindow",          "maxContextWindow", "maxTokens",        "reasoning",
    "input", "contextPromotionTarget", "compactionModel",  "preferWebsockets", "cost",
};

template <typename E, std::size_t N>
std::optional<E> parse_in(const std::array<std::string_view, N>& names, std::string_view s) noexcept {
    for (std::size_t i = 0; i < N; ++i) {
        if (names[i] == s) return static_cast<E>(i);
    }
    return std::nullopt;
}

template <std::size_t N>
bool contains(const std::array<std::string_view, N>& names, std::string_view s) noexcept {
    return std::find(names.begin(), names.end(), s) != names.end();
}

bool is_upper(char c) noexcept { return c >= 'A' && c <= 'Z'; }
bool is_digit(char c) noexcept { return c >= '0' && c <= '9'; }
bool is_alnum(char c) noexcept { return is_upper(c) || is_digit(c) || (c >= 'a' && c <= 'z'); }

Yaml key_reference_yaml(const KeyReference& k) {
    if (k.kind == KeyReference::Kind::EnvVar) return ystr(k.value);
    return ystr("!" + k.value);
}

ApiKeySetting api_key_from_yaml(std::string_view value) {
    if (value.starts_with('!')) return ApiKeySetting::command();
    if (is_env_var_name(value)) return ApiKeySetting::env_var(std::string(value));
    return ApiKeySetting::literal();
}

Result<> non_empty(std::string_view field, std::string_view s) {
    if (detail::trim(s).empty() || detail::any_char(s, detail::is_control)) {
        return std::unexpected(Error::invalid(std::string(field), "must be a non-empty single line"));
    }
    return {};
}

Result<Yaml> positive_int(std::string_view field, std::uint64_t n) {
    if (n > 0 && n <= (std::uint64_t{1} << 53)) return Yaml(static_cast<std::int64_t>(n));
    return std::unexpected(Error::invalid(std::string(field), "must be a positive integer"));
}

Result<Yaml> modalities_yaml(const std::vector<Modality>& m) {
    if (std::find(m.begin(), m.end(), Modality::Text) == m.end()) {
        return std::unexpected(Error::invalid("input", "must include text"));
    }
    std::vector<Modality> seen;
    for (Modality x : m) {
        if (std::find(seen.begin(), seen.end(), x) == seen.end()) seen.push_back(x);
    }
    yaml::Array items;
    for (Modality x : seen) items.emplace_back(as_str(x));
    return Yaml(std::move(items));
}

Result<Yaml> number_yaml(std::string_view field, double v) {
    if (!std::isfinite(v) || v < 0.0) {
        return std::unexpected(Error::invalid(std::string(field), "must be a non-negative number"));
    }
    if (std::trunc(v) == v && v < 9.0e15) {
        // Whole numbers are written as YAML integers; the conversion is exact below 2^53.
        return Yaml(static_cast<std::int64_t>(v));
    }
    // Rust's `{}` Display of f64: shortest round trip digits, never an exponent.
    std::array<char, 400> buf{};
    auto [end, ec] = std::to_chars(buf.data(), buf.data() + buf.size(), v, std::chars_format::fixed);
    return Yaml(Yaml::Real{std::string(buf.data(), ec == std::errc{} ? end : buf.data())});
}

Result<Yaml> cost_yaml(const ModelCost& c) {
    auto input = number_yaml("cost.input", c.input);
    if (!input) return std::unexpected(input.error());
    auto output = number_yaml("cost.output", c.output);
    if (!output) return std::unexpected(output.error());
    auto cache_read = number_yaml("cost.cacheRead", c.cache_read);
    if (!cache_read) return std::unexpected(cache_read.error());
    auto cache_write = number_yaml("cost.cacheWrite", c.cache_write);
    if (!cache_write) return std::unexpected(cache_write.error());
    return ymap({
        {"input", std::move(*input)},
        {"output", std::move(*output)},
        {"cacheRead", std::move(*cache_read)},
        {"cacheWrite", std::move(*cache_write)},
    });
}

std::optional<ModelCost> read_cost(const Yaml& y) {
    if (!y.as_hash()) return std::nullopt;
    auto n = [&](std::string_view k) -> double {
        const Yaml* v = y.get(k);
        if (!v) return 0.0;
        if (const auto* i = std::get_if<std::int64_t>(&v->storage())) return static_cast<double>(*i);
        return v->as_f64().value_or(0.0);
    };
    return ModelCost{n("input"), n("output"), n("cacheRead"), n("cacheWrite")};
}

std::optional<std::uint64_t> read_u64(const Yaml* y) {
    if (!y) return std::nullopt;
    auto v = y->as_i64();
    if (!v || *v < 0) return std::nullopt;
    return static_cast<std::uint64_t>(*v);
}

std::optional<std::string> read_str(const Yaml* y) {
    if (!y) return std::nullopt;
    if (const auto* s = y->as_str()) return *s;
    return std::nullopt;
}

std::optional<bool> read_bool(const Yaml* y) {
    if (!y) return std::nullopt;
    return y->as_bool();
}

std::optional<std::vector<Modality>> read_modalities(const Yaml* y) {
    if (!y) return std::nullopt;
    const auto* v = y->as_vec();
    if (!v) return std::nullopt;
    std::vector<Modality> out;
    for (const Yaml& item : *v) {
        const auto* s = item.as_str();
        if (!s) continue;
        if (auto m = parse_modality(*s)) out.push_back(*m);
    }
    return out;
}

std::optional<ModelCost> read_cost_at(const Yaml* y) {
    if (!y) return std::nullopt;
    return read_cost(*y);
}

std::optional<CustomModel> read_model(const Yaml& y) {
    if (!y.as_hash()) return std::nullopt;
    auto id = read_str(y.get("id"));
    if (!id) return std::nullopt;
    return CustomModel{
        .id = std::move(*id),
        .name = read_str(y.get("name")),
        .api = read_str(y.get("api")),
        .reasoning = read_bool(y.get("reasoning")),
        .input = read_modalities(y.get("input")),
        .context_window = read_u64(y.get("contextWindow")),
        .max_tokens = read_u64(y.get("maxTokens")),
        .cost = read_cost_at(y.get("cost")),
    };
}

Result<> check_api(std::string_view api) {
    if (contains(APIS, api)) return {};
    return std::unexpected(Error::invalid("api", "`" + std::string(api) + "` is not an api omp knows"));
}

Result<Yaml> model_yaml(const CustomModel& m) {
    if (auto r = non_empty("models[].id", m.id); !r) return std::unexpected(r.error());
    std::vector<std::pair<std::string, Yaml>> pairs;
    pairs.emplace_back("id", ystr(m.id));
    if (m.name) {
        if (auto r = non_empty("models[].name", *m.name); !r) return std::unexpected(r.error());
        pairs.emplace_back("name", ystr(*m.name));
    }
    if (m.api) {
        if (auto r = check_api(*m.api); !r) return std::unexpected(r.error());
        pairs.emplace_back("api", ystr(*m.api));
    }
    if (m.reasoning) pairs.emplace_back("reasoning", Yaml(*m.reasoning));
    if (m.input) {
        auto y = modalities_yaml(*m.input);
        if (!y) return std::unexpected(y.error());
        pairs.emplace_back("input", std::move(*y));
    }
    if (m.context_window) {
        auto y = positive_int("contextWindow", *m.context_window);
        if (!y) return std::unexpected(y.error());
        pairs.emplace_back("contextWindow", std::move(*y));
    }
    if (m.max_tokens) {
        auto y = positive_int("maxTokens", *m.max_tokens);
        if (!y) return std::unexpected(y.error());
        pairs.emplace_back("maxTokens", std::move(*y));
    }
    if (m.cost) {
        auto y = cost_yaml(*m.cost);
        if (!y) return std::unexpected(y.error());
        pairs.emplace_back("cost", std::move(*y));
    }
    return ymap(std::move(pairs));
}

Result<> check_provider_id(std::string_view id) {
    if (!is_provider_id(id)) {
        return std::unexpected(
            Error::invalid("provider id", "`" + std::string(id) + "` must be letters, digits, `-`, `_` or `.`"));
    }
    return {};
}

ModelOverride read_override(const Yaml& v) {
    const auto* h = v.as_hash();
    if (!h) return {};
    ModelOverride o{
        .name = read_str(v.get("name")),
        .context_window = read_u64(v.get("contextWindow")),
        .max_context_window = read_u64(v.get("maxContextWindow")),
        .max_tokens = read_u64(v.get("maxTokens")),
        .reasoning = read_bool(v.get("reasoning")),
        .input = read_modalities(v.get("input")),
        .context_promotion_target = read_str(v.get("contextPromotionTarget")),
        .compaction_model = read_str(v.get("compactionModel")),
        .prefer_websockets = read_bool(v.get("preferWebsockets")),
        .cost = read_cost_at(v.get("cost")),
        .other = {},
    };
    for (const auto& [k, _] : *h) {
        const auto* s = k.as_str();
        if (s && !contains(OVERRIDE_KEYS, *s)) o.other.push_back(*s);
    }
    return o;
}

ProviderEntry read_provider(std::string_view id, const Yaml& v) {
    const bool is_map = v.as_hash() != nullptr;
    auto get = [&](std::string_view k) -> const Yaml* { return is_map ? v.get(k) : nullptr; };
    ProviderEntry e{.id = std::string(id)};
    if (const Yaml* m = get("modelOverrides"); m && m->as_hash()) {
        for (const auto& [k, o] : *m->as_hash()) {
            if (const auto* s = k.as_str()) e.overrides.emplace_back(*s, read_override(o));
        }
    }
    e.base_url = read_str(get("baseUrl"));
    e.api = read_str(get("api"));
    if (const Yaml* k = get("apiKey"); k && k->as_str()) e.api_key = api_key_from_yaml(*k->as_str());
    if (const Yaml* a = get("auth"); a && a->as_str()) e.auth = parse_auth_mode(*a->as_str());
    e.auth_header = read_bool(get("authHeader"));
    if (const Yaml* d = get("discovery"); d && d->as_hash()) e.discovery = read_str(d->get("type"));
    if (const Yaml* ms = get("models"); ms && ms->as_vec()) {
        for (const Yaml& m : *ms->as_vec()) {
            if (auto model = read_model(m)) e.models.push_back(std::move(*model));
        }
    }
    return e;
}

} // namespace

std::string_view as_str(AuthMode m) noexcept { return AUTH_MODES[static_cast<std::size_t>(m)]; }
std::optional<AuthMode> parse_auth_mode(std::string_view s) noexcept { return parse_in<AuthMode>(AUTH_MODES, s); }
std::string_view as_str(Modality m) noexcept { return MODALITIES[static_cast<std::size_t>(m)]; }
std::optional<Modality> parse_modality(std::string_view s) noexcept { return parse_in<Modality>(MODALITIES, s); }
std::string_view as_str(OverrideKey k) noexcept { return OVERRIDE_KEYS[static_cast<std::size_t>(k)]; }
std::optional<OverrideKey> parse_override_key(std::string_view s) noexcept {
    return parse_in<OverrideKey>(OVERRIDE_KEYS, s);
}

Result<KeyReference> KeyReference::env_var(std::string_view name) {
    if (!is_env_var_name(name)) {
        return std::unexpected(
            Error::invalid("apiKey", "an environment variable name is upper case letters, digits and `_`"));
    }
    return KeyReference{Kind::EnvVar, std::string(name)};
}

Result<KeyReference> KeyReference::command(std::string_view command) {
    command = detail::trim(command);
    if (command.starts_with('!')) command.remove_prefix(1);
    command = detail::trim(command);
    if (command.empty() || command.find_first_of("\n\r") != std::string_view::npos) {
        return std::unexpected(Error::invalid("apiKey", "a key command is one non-empty line"));
    }
    return KeyReference{Kind::Command, std::string(command)};
}

bool is_env_var_name(std::string_view name) noexcept {
    if (name.empty() || !(is_upper(name[0]) || name[0] == '_')) return false;
    return std::all_of(name.begin() + 1, name.end(), [](char c) { return is_upper(c) || is_digit(c) || c == '_'; });
}

bool is_provider_id(std::string_view id) noexcept {
    return !id.empty() &&
           std::all_of(id.begin(), id.end(), [](char c) { return is_alnum(c) || c == '-' || c == '_' || c == '.'; });
}

Result<Yaml> OverrideField::yaml() const {
    const std::string_view field = as_str(key_);
    return std::visit(
        [&](const auto& v) -> Result<Yaml> {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, std::string>) {
                if (auto r = non_empty(field, v); !r) return std::unexpected(r.error());
                return ystr(v);
            } else if constexpr (std::is_same_v<T, std::uint64_t>) {
                return positive_int(field, v);
            } else if constexpr (std::is_same_v<T, bool>) {
                return Yaml(v);
            } else if constexpr (std::is_same_v<T, std::vector<Modality>>) {
                return modalities_yaml(v);
            } else {
                return cost_yaml(v);
            }
        },
        value_);
}

Result<ModelsYml> ModelsYml::load(const std::filesystem::path& path) {
    auto loaded = load_snapshot(path);
    if (!loaded) return std::unexpected(loaded.error());
    auto& [snapshot, text] = *loaded;
    auto doc = YamlDocument::parse(text ? std::string_view(*text) : std::string_view(), path.string());
    if (!doc) return std::unexpected(doc.error());
    return ModelsYml(std::move(*doc), std::move(snapshot));
}

Result<> ModelsYml::save() { return save_snapshot(snapshot_, doc_.text()); }

contracts::Awaitable<Result<>> ModelsYml::save_validated(const SettingsValidator& validator) {
    auto checked = co_await validate(validator);
    if (!checked) co_return checked;
    co_return save();
}

contracts::Awaitable<Result<>> ModelsYml::validate(const SettingsValidator& validator) const {
    co_return co_await validator.check_models(doc_.text());
}

std::vector<ProviderEntry> ModelsYml::providers() const {
    std::vector<ProviderEntry> out;
    const Yaml* map = doc_.get({"providers"});
    if (!map || !map->as_hash()) return out;
    for (const auto& [k, v] : *map->as_hash()) {
        if (const auto* id = k.as_str()) out.push_back(read_provider(*id, v));
    }
    return out;
}

std::optional<ProviderEntry> ModelsYml::provider(std::string_view id) const {
    const Yaml* v = doc_.get({"providers", id});
    if (!v) return std::nullopt;
    return read_provider(id, *v);
}

Result<> ModelsYml::set_custom_provider(const CustomProvider& p) {
    if (auto r = check_provider_id(p.id); !r) return r;
    if (p.base_url) {
        if (auto r = non_empty("baseUrl", *p.base_url); !r) return r;
        if (!(p.base_url->starts_with("http://") || p.base_url->starts_with("https://"))) {
            return std::unexpected(Error::invalid("baseUrl", "must be an http or https URL"));
        }
    }
    if (p.api) {
        if (auto r = check_api(*p.api); !r) return r;
    }
    if (p.discovery && !contains(DISCOVERY_TYPES, *p.discovery)) {
        return std::unexpected(
            Error::invalid("discovery", "`" + *p.discovery + "` is not a discovery type omp knows"));
    }
    yaml::Array models;
    for (const CustomModel& m : p.models) {
        auto y = model_yaml(m);
        if (!y) return std::unexpected(y.error());
        models.push_back(std::move(*y));
    }

    YamlDocument next = doc_;
    const std::string_view id = p.id;
    auto put = [&](std::string_view key, std::optional<Yaml> value) -> Result<> {
        const std::array<std::string_view, 3> path = {"providers", id, key};
        if (value) return next.set(path, std::move(*value));
        auto removed = next.remove(path);
        if (!removed) return std::unexpected(removed.error());
        return {};
    };
    auto opt_str = [](const std::optional<std::string>& s) -> std::optional<Yaml> {
        if (!s) return std::nullopt;
        return ystr(*s);
    };
    if (auto r = put("baseUrl", opt_str(p.base_url)); !r) return r;
    if (auto r = put("api", opt_str(p.api)); !r) return r;
    switch (p.api_key.kind) {
    case ApiKeyWrite::Kind::Remove:
        if (auto r = put("apiKey", std::nullopt); !r) return r;
        break;
    case ApiKeyWrite::Kind::Keep:
        break;
    case ApiKeyWrite::Kind::Set:
        if (auto r = put("apiKey", key_reference_yaml(p.api_key.key)); !r) return r;
        break;
    }
    if (auto r = put("auth", p.auth ? std::optional<Yaml>(ystr(std::string(as_str(*p.auth)))) : std::nullopt); !r) return r;
    if (auto r = put("authHeader", p.auth_header ? std::optional<Yaml>(Yaml(*p.auth_header)) : std::nullopt); !r) return r;
    if (auto r = put("discovery", p.discovery ? std::optional<Yaml>(ymap({{"type", ystr(*p.discovery)}})) : std::nullopt);
        !r) {
        return r;
    }
    if (auto r = put("models", models.empty() ? std::nullopt : std::optional<Yaml>(Yaml(std::move(models)))); !r) return r;
    doc_ = std::move(next);
    return {};
}

Result<bool> ModelsYml::remove_provider(std::string_view id) { return doc_.remove({"providers", id}); }

Result<> ModelsYml::set_override(std::string_view provider, std::string_view model, const OverrideField& field) {
    if (auto r = check_provider_id(provider); !r) return r;
    if (auto r = non_empty("model id", model); !r) return r;
    auto value = field.yaml();
    if (!value) return std::unexpected(value.error());
    YamlDocument next = doc_;
    if (auto r = next.set({"providers", provider, "modelOverrides", model, as_str(field.key())}, std::move(*value)); !r) {
        return r;
    }
    doc_ = std::move(next);
    return {};
}

Result<bool> ModelsYml::clear_override(std::string_view provider, std::string_view model, OverrideKey key) {
    YamlDocument next = doc_;
    auto removed = next.remove({"providers", provider, "modelOverrides", model, as_str(key)});
    if (!removed) return removed;
    doc_ = std::move(next);
    return removed;
}

} // namespace loa::settings::models::models_yml
