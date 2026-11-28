// Port of the state half of `crates/loa-ui-settings/src/models/settings.rs`.

#include "loa/ui/settings/models/settings.hpp"

#include <algorithm>
#include <charconv>
#include <format>

namespace loa::ui::settings::models::settings {

namespace vm = loa::view::settings::models;
using kit::widgets::Field;

namespace {

constexpr std::string_view placeholder(OverrideKey k) {
    switch (k) {
    case OverrideKey::ContextWindow:
    case OverrideKey::MaxContextWindow:
    case OverrideKey::MaxTokens: return "whole number";
    case OverrideKey::Reasoning:
    case OverrideKey::PreferWebsockets: return "true or false";
    default: return "";
    }
}

constexpr std::array<Field, OVERRIDE_FIELDS.size()> OVERRIDE_FORM = [] {
    std::array<Field, OVERRIDE_FIELDS.size()> out{};
    for (std::size_t i = 0; i < OVERRIDE_FIELDS.size(); ++i) out[i] = {OVERRIDE_FIELDS[i].first, OVERRIDE_FIELDS[i].first, placeholder(OVERRIDE_FIELDS[i].second)};
    return out;
}();

constexpr std::array<Field, 5> PROVIDER_FORM{{
    {"id", "Id", "my-provider"},
    {"base_url", "Base URL", "https://example.com/v1"},
    {"api", "API", "openai-completions"},
    {"api_key_env", "API key variable", "MY_PROVIDER_API_KEY"},
    {"models", "Models", "model-a, model-b"},
}};

std::string_view trim(std::string_view s) {
    constexpr std::string_view ws = " \t\n\r\f\v";
    const auto b = s.find_first_not_of(ws);
    if (b == std::string_view::npos) return {};
    return s.substr(b, s.find_last_not_of(ws) - b + 1);
}

std::optional<std::size_t> position(std::span<const Field> fields, std::string_view key) {
    for (std::size_t i = 0; i < fields.size(); ++i)
        if (fields[i].key == key) return i;
    return std::nullopt;
}

std::string get(std::span<const Field> fields, std::span<const std::string> values, std::string_view key) {
    const auto i = position(fields, key);
    return i ? values[*i] : std::string();
}

std::expected<void, std::string> set(std::span<const Field> fields, std::span<std::string> values, std::string_view key, std::string_view value) {
    const auto i = position(fields, key);
    if (!i) return std::unexpected(std::format("no field {}", key));
    values[*i] = std::string(value);
    return {};
}

std::string to_text(const FieldValue& v) {
    return std::visit(
        [](const auto& x) -> std::string {
            using T = std::decay_t<decltype(x)>;
            if constexpr (std::is_same_v<T, std::string>) return x;
            else if constexpr (std::is_same_v<T, bool>) return x ? "true" : "false";
            else return std::format("{}", x);
        },
        v);
}

template <typename T> void move_item(std::vector<T>& items, std::size_t from, std::size_t to) {
    if (from < items.size() && to < items.size()) {
        auto item = std::move(items[from]);
        items.erase(items.begin() + static_cast<std::ptrdiff_t>(from));
        items.insert(items.begin() + static_cast<std::ptrdiff_t>(to), std::move(item));
    }
}

template <typename T> void remove(std::vector<T>& items, std::size_t i) {
    if (i < items.size()) items.erase(items.begin() + static_cast<std::ptrdiff_t>(i));
}

template <typename T, typename F> void at(std::vector<T>& items, std::size_t i, F&& f) {
    if (i < items.size()) f(items[i]);
}

} // namespace

std::span<const Field> override_form() { return OVERRIDE_FORM; }
std::span<const Field> provider_form() { return PROVIDER_FORM; }

std::expected<std::optional<FieldValue>, std::string> parse(OverrideKey key, std::string_view raw) {
    raw = trim(raw);
    if (raw.empty()) return std::optional<FieldValue>{};
    switch (key) {
    case OverrideKey::ContextWindow:
    case OverrideKey::MaxContextWindow:
    case OverrideKey::MaxTokens: {
        // Rust `i64::from_str`: an optional sign, then digits only.
        auto digits = raw;
        if (digits.starts_with('+')) digits.remove_prefix(1);
        std::int64_t n = 0;
        const auto [end, ec] = std::from_chars(digits.data(), digits.data() + digits.size(), n);
        if (digits.empty() || ec != std::errc{} || end != digits.data() + digits.size() || n <= 0)
            return std::unexpected(std::format("\"{}\" is not a whole number above zero", raw));
        return std::optional<FieldValue>{FieldValue{n}};
    }
    case OverrideKey::Reasoning:
    case OverrideKey::PreferWebsockets:
        if (raw == "true") return std::optional<FieldValue>{FieldValue{true}};
        if (raw == "false") return std::optional<FieldValue>{FieldValue{false}};
        return std::unexpected(std::format("\"{}\" is not true or false", raw));
    default: return std::optional<FieldValue>{FieldValue{std::string(raw)}};
    }
}

OverrideDraft::OverrideDraft(const vm::ModelOverride& o) : selector(o.selector) {
    values.reserve(OVERRIDE_FIELDS.size());
    for (const auto& [key, _] : OVERRIDE_FIELDS) {
        const auto it = std::ranges::find_if(o.fields, [key](const OverrideField& f) { return f.key == key; });
        values.push_back(it == o.fields.end() ? std::string() : to_text(it->value));
    }
}

std::pair<std::vector<OverrideField>, Errors> OverrideDraft::parsed() const {
    std::vector<OverrideField> fields;
    Errors errors;
    for (std::size_t i = 0; i < OVERRIDE_FIELDS.size() && i < values.size(); ++i) {
        const auto& [key, k] = OVERRIDE_FIELDS[i];
        auto value = parse(k, values[i]);
        if (!value) errors.insert_or_assign(std::string(key), std::move(value.error()));
        else if (*value) fields.push_back({std::string(key), std::move(**value)});
    }
    return {std::move(fields), std::move(errors)};
}

std::string OverrideDraft::get(std::string_view key) const { return settings::get(OVERRIDE_FORM, values, key); }
std::expected<void, std::string> OverrideDraft::set(std::string_view key, std::string_view value) {
    return settings::set(OVERRIDE_FORM, values, key, value);
}

ProviderDraft::ProviderDraft(const CustomProvider& p) : auth(p.auth) {
    std::string models;
    for (const auto& m : p.models) models += (models.empty() ? "" : ", ") + m;
    values = {p.id, p.base_url, p.api, p.api_key_env, std::move(models)};
}

CustomProvider ProviderDraft::provider() const {
    CustomProvider p{std::string(trim(values[0])), std::string(trim(values[1])), std::string(trim(values[2])), auth,
                     std::string(trim(values[3])), {}};
    std::string_view rest = trim(values[4]);
    while (true) {
        const auto comma = rest.find(',');
        const auto m = trim(rest.substr(0, comma));
        if (!m.empty()) p.models.emplace_back(m);
        if (comma == std::string_view::npos) break;
        rest.remove_prefix(comma + 1);
    }
    return p;
}

std::string ProviderDraft::get(std::string_view key) const { return settings::get(PROVIDER_FORM, values, key); }
std::expected<void, std::string> ProviderDraft::set(std::string_view key, std::string_view value) {
    return settings::set(PROVIDER_FORM, values, key, value);
}

Errors provider_errors(const CustomProvider& p, std::span<const CustomProvider> others) {
    Errors errors;
    const std::string_view id = p.id;
    if (id.empty()) errors.emplace("id", "Provider id is empty");
    else if (!vm::is_provider_id(id) || std::ranges::any_of(id, [](char c) { return c >= 'A' && c <= 'Z'; }))
        errors.emplace("id", "Provider id may only contain a-z, 0-9, -, _ and .");
    else if (std::ranges::any_of(others, [id](const CustomProvider& o) { return o.id == id; }))
        errors.emplace("id", std::format("Another custom provider already uses the id \"{}\"", id));
    if (!p.api_key_env.empty() && !vm::is_env_var_name(p.api_key_env))
        errors.emplace("api_key_env", "API key must be the name of an environment variable (A-Z, 0-9, _), not the key itself");
    return errors;
}

Draft::Draft(const ModelSettings& s) : base(s) {
    base.overrides.clear();
    base.custom_providers.clear();
    for (const auto& o : s.overrides) overrides.emplace_back(o);
    for (const auto& p : s.custom_providers) custom.emplace_back(p);
}

std::vector<CustomProvider> Draft::providers() const {
    std::vector<CustomProvider> out;
    for (const auto& p : custom) out.push_back(p.provider());
    return out;
}

std::vector<Errors> Draft::provider_errors() const {
    const auto providers = this->providers();
    std::vector<Errors> out;
    for (std::size_t i = 0; i < providers.size(); ++i) {
        std::vector<CustomProvider> others;
        for (std::size_t j = 0; j < providers.size(); ++j)
            if (j != i) others.push_back(providers[j]);
        out.push_back(settings::provider_errors(providers[i], others));
    }
    return out;
}

std::expected<ModelSettings, std::vector<std::string>> Draft::settings() const {
    std::vector<std::string> problems;
    std::vector<vm::RoleChain> roles;
    for (const auto& r : base.roles) roles.push_back({std::string(trim(r.role)), r.chain});
    for (std::size_t i = 0; i < roles.size(); ++i) {
        const auto& name = roles[i].role;
        if (name.empty()) problems.push_back(std::format("Role {} has no name", i + 1));
        else if (std::any_of(roles.begin(), roles.begin() + static_cast<std::ptrdiff_t>(i), [&](const vm::RoleChain& r) { return r.role == name; }))
            problems.push_back(std::format("Role \"{}\" is listed twice", name));
    }
    std::vector<vm::ModelOverride> out_overrides;
    for (const auto& o : overrides) {
        auto [fields, errors] = o.parsed();
        for (const auto& [key, e] : errors) problems.push_back(std::format("{}: {}: {}", o.selector, key, e));
        out_overrides.push_back({o.selector, std::move(fields)});
    }
    auto custom_providers = providers();
    const auto errors = provider_errors();
    for (std::size_t i = 0; i < custom_providers.size(); ++i) {
        const auto& p = custom_providers[i];
        const auto name = p.id.empty() ? std::format("Custom provider {}", i + 1) : p.id;
        for (const auto& [_, e] : errors[i]) problems.push_back(std::format("{}: {}", name, e));
    }
    if (!problems.empty()) return std::unexpected(std::move(problems));
    ModelSettings s = base;
    s.roles = std::move(roles);
    s.overrides = std::move(out_overrides);
    s.custom_providers = std::move(custom_providers);
    return s;
}

State::State(ModelSettings settings, std::vector<std::string> known) : State(Draft(settings), std::move(known)) {}

State::State(Draft draft, std::vector<std::string> known)
    : baseline_(draft), draft_(std::move(draft)), known_(std::move(known)), chain_inputs_(draft_.base.roles.size()) {}

void State::saved(const ModelSettings& settings) { *this = State(Draft(settings), std::move(known_)); }

void State::reset() { *this = State(Draft(baseline_), std::move(known_)); }

std::optional<Intent> State::reduce(Local m) {
    auto& d = draft_;
    return std::visit(
        [&](auto&& l) -> std::optional<Intent> {
            using T = std::decay_t<decltype(l)>;
            if constexpr (std::is_same_v<T, local::NewRole>) {
                new_role_ = std::move(l.name);
            } else if constexpr (std::is_same_v<T, local::AddRole>) {
                const auto name = trim(new_role_);
                if (!name.empty()) {
                    d.base.roles.push_back({std::string(name), {}});
                    chain_inputs_.emplace_back();
                    new_role_.clear();
                }
            } else if constexpr (std::is_same_v<T, local::RemoveRole>) {
                remove(d.base.roles, l.role);
                remove(chain_inputs_, l.role);
            } else if constexpr (std::is_same_v<T, local::RenameRole>) {
                at(d.base.roles, l.role, [&](vm::RoleChain& r) { r.role = std::move(l.name); });
            } else if constexpr (std::is_same_v<T, local::ChainInput>) {
                at(chain_inputs_, l.role, [&](std::string& input) { input = std::move(l.value); });
            } else if constexpr (std::is_same_v<T, local::AddChain>) {
                std::string selector;
                if (l.picked) selector = std::move(*l.picked);
                else if (l.role < chain_inputs_.size()) selector = std::exchange(chain_inputs_[l.role], {});
                const std::string s(trim(selector));
                if (l.role < d.base.roles.size() && !s.empty()) {
                    auto& chain = d.base.roles[l.role].chain;
                    if (std::ranges::find(chain, s) == chain.end()) chain.push_back(s);
                }
            } else if constexpr (std::is_same_v<T, local::RemoveChain>) {
                at(d.base.roles, l.role, [&](vm::RoleChain& r) { remove(r.chain, l.entry); });
            } else if constexpr (std::is_same_v<T, local::MoveChain>) {
                at(d.base.roles, l.role, [&](vm::RoleChain& r) { move_item(r.chain, l.from, l.to); });
            } else if constexpr (std::is_same_v<T, local::ModelToggled>) {
                at(d.base.models, l.model, [&](vm::ModelToggle& t) { t.enabled = l.on; });
            } else if constexpr (std::is_same_v<T, local::ProviderToggled>) {
                at(d.base.providers, l.provider, [&](vm::ProviderToggle& t) { t.enabled = l.on; });
            } else if constexpr (std::is_same_v<T, local::MoveProvider>) {
                move_item(d.base.providers, l.from, l.to);
            } else if constexpr (std::is_same_v<T, local::NewOverride>) {
                new_override_ = std::move(l.selector);
            } else if constexpr (std::is_same_v<T, local::AddOverride>) {
                const std::string picked = l.picked ? std::move(*l.picked) : std::exchange(new_override_, {});
                const std::string s(trim(picked));
                if (!s.empty() && std::ranges::none_of(d.overrides, [&](const OverrideDraft& o) { return o.selector == s; }))
                    d.overrides.emplace_back(s, std::vector<std::string>(OVERRIDE_FIELDS.size()));
            } else if constexpr (std::is_same_v<T, local::RemoveOverride>) {
                remove(d.overrides, l.index);
            } else if constexpr (std::is_same_v<T, local::OverrideEdit>) {
                at(d.overrides, l.index, [&](OverrideDraft& o) { (void)o.set(l.key, l.value); });
            } else if constexpr (std::is_same_v<T, local::AddProvider>) {
                d.custom.emplace_back(CustomProvider{});
            } else if constexpr (std::is_same_v<T, local::RemoveProvider>) {
                remove(d.custom, l.index);
            } else if constexpr (std::is_same_v<T, local::ProviderEdit>) {
                at(d.custom, l.index, [&](ProviderDraft& p) { (void)p.set(l.key, l.value); });
            } else if constexpr (std::is_same_v<T, local::ProviderAuth>) {
                at(d.custom, l.index, [&](ProviderDraft& p) { p.auth = l.auth; });
            } else if constexpr (std::is_same_v<T, local::HarnessModel>) {
                at(d.base.harnesses, l.index, [&](vm::HarnessModel& h) { h.model = std::move(l.model); });
            } else if constexpr (std::is_same_v<T, local::HarnessEffort>) {
                at(d.base.harnesses, l.index, [&](vm::HarnessModel& h) { h.effort = l.effort == HARNESS_DEFAULT ? std::string() : std::move(l.effort); });
            } else if constexpr (std::is_same_v<T, local::Save>) {
                app_errors_.clear();
                auto settings = d.settings();
                if (settings) {
                    local_errors_.clear();
                    return vm::settings::intent::Save{std::move(*settings)};
                }
                local_errors_ = std::move(settings.error());
            } else {
                reset();
                return vm::settings::intent::Reverted{};
            }
            return std::nullopt;
        },
        std::move(m));
}

} // namespace loa::ui::settings::models::settings
