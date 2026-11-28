#include "loa/settings/models/settings.hpp"

#include <algorithm>
#include <limits>
#include <map>
#include <utility>
#include <optional>

#include "loa/settings/models/selector.hpp"
#include "text_util.hpp"

namespace loa::settings::models::settings {

using config::ChainKey;
using config::OmpConfig;
using config::ScopedEntry;
using config::ScopedList;
using models_yml::ApiKeySetting;
using models_yml::ApiKeyWrite;
using models_yml::KeyReference;
using models_yml::ModelsYml;
using models_yml::OverrideKey;
using selector::ModelRef;
using selector::Selector;

namespace {

using Step = std::expected<bool, ValidationError>;

ValidationError error(std::string location, std::string message) { return {std::move(location), std::move(message)}; }
ValidationError error(std::string location, const Error& e) { return {std::move(location), e.to_string()}; }

// Maps a models `Result<T>` to a step result at `location`.
Step step(std::string_view location, const Result<bool>& r) {
    if (r) return *r;
    return std::unexpected(error(std::string(location), r.error()));
}
Step step(std::string_view location, const Result<>& r) {
    if (r) return true;
    return std::unexpected(error(std::string(location), r.error()));
}

std::expected<std::vector<RoleChain>, std::string> roles(const OmpConfig& config) {
    auto chains = config.fallback_chains();
    if (!chains) return std::unexpected(chains.error().to_string());
    auto assignments = config.model_roles();
    if (!assignments) return std::unexpected(assignments.error().to_string());
    std::vector<RoleChain> out;
    for (auto& assignment : *assignments) {
        std::string primary;
        for (std::size_t i = 0; i < assignment.candidates.size(); ++i) {
            if (i > 0) primary += ',';
            primary += assignment.candidates[i].to_string();
        }
        std::vector<std::string> chain{std::move(primary)};
        const ChainKey key = ChainKey::role(assignment.role);
        auto it = std::find_if(chains->begin(), chains->end(), [&](const auto& c) { return c.key == key; });
        if (it != chains->end()) {
            for (const Selector& s : it->fallbacks) chain.push_back(s.to_string());
        }
        out.push_back(RoleChain{std::move(assignment.role), std::move(chain)});
    }
    return out;
}

std::vector<std::string_view> everywhere(const std::vector<ScopedEntry>& entries) {
    std::vector<std::string_view> out;
    for (const ScopedEntry& e : entries) {
        if (e.is_everywhere()) out.emplace_back(e.value);
    }
    return out;
}

std::vector<ModelToggle> model_toggles(const OmpConfig& config, std::span<const rpc::ModelInfo> available) {
    const auto entries = config.scoped_list(ScopedList::EnabledModels);
    const auto patterns = everywhere(entries);
    std::vector<ModelToggle> toggles;
    for (const rpc::ModelInfo& m : available) {
        const bool enabled = patterns.empty() || std::any_of(patterns.begin(), patterns.end(), [&](std::string_view p) {
                                 return pattern_matches(p, m.provider, m.id);
                             });
        toggles.push_back(ModelToggle{m.selector(), enabled});
    }
    // Exact selectors of models omp does not list now (signed out provider, stopped
    // local server) stay visible so they can be turned off.
    for (std::string_view pattern : patterns) {
        const bool exact = pattern.find('/') != std::string_view::npos && pattern.find_first_of("*?") == std::string_view::npos;
        if (exact && std::none_of(toggles.begin(), toggles.end(), [&](const ModelToggle& t) { return t.selector == pattern; })) {
            toggles.push_back(ModelToggle{std::string(pattern), true});
        }
    }
    return toggles;
}

std::vector<ProviderToggle> provider_toggles(const OmpConfig& config, std::span<const rpc::ModelInfo> available,
                                             const std::unordered_map<std::string, std::string>& provider_names) {
    const auto disabled_entries = config.scoped_list(ScopedList::DisabledProviders);
    const auto disabled = everywhere(disabled_entries);
    std::vector<std::string> ids = config.provider_order();
    auto add = [&](std::string_view id) {
        if (std::find(ids.begin(), ids.end(), id) == ids.end()) ids.emplace_back(id);
    };
    for (const rpc::ModelInfo& m : available) add(m.provider);
    for (std::string_view id : disabled) add(id);
    std::vector<ProviderToggle> out;
    for (std::string& id : ids) {
        auto name = provider_names.find(id);
        const bool enabled = std::find(disabled.begin(), disabled.end(), id) == disabled.end();
        out.push_back(ProviderToggle{
            .id = std::move(id),
            .name = name == provider_names.end() ? std::string() : name->second,
            .enabled = enabled,
        });
    }
    return out;
}

std::vector<OverrideField> override_fields(const models_yml::ModelOverride& o) {
    auto integer = [](const std::optional<std::uint64_t>& v) -> std::optional<FieldValue> {
        if (!v) return std::nullopt;
        const auto max = static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max());
        return FieldValue::integer(static_cast<std::int64_t>(std::min(*v, max)));
    };
    auto text = [](const std::optional<std::string>& v) -> std::optional<FieldValue> {
        if (!v) return std::nullopt;
        return FieldValue::text(*v);
    };
    auto flag = [](const std::optional<bool>& v) -> std::optional<FieldValue> {
        if (!v) return std::nullopt;
        return FieldValue::boolean(*v);
    };
    const std::array<std::optional<FieldValue>, OVERRIDE_FIELDS.size()> values = {
        text(o.name),
        integer(o.context_window),
        integer(o.max_context_window),
        integer(o.max_tokens),
        flag(o.reasoning),
        text(o.context_promotion_target),
        text(o.compaction_model),
        flag(o.prefer_websockets),
    };
    std::vector<OverrideField> out;
    for (std::size_t i = 0; i < OVERRIDE_FIELDS.size(); ++i) {
        if (values[i]) out.push_back(OverrideField{std::string(OVERRIDE_FIELDS[i].first), *values[i]});
    }
    return out;
}

std::vector<ModelOverride> overrides(const ModelsYml& models) {
    std::vector<ModelOverride> out;
    for (const auto& entry : models.providers()) {
        for (const auto& [model, o] : entry.overrides) {
            out.push_back(ModelOverride{entry.id + "/" + model, override_fields(o)});
        }
    }
    return out;
}

bool is_custom(const models_yml::ProviderEntry& entry) {
    return entry.base_url || entry.api || entry.discovery || !entry.models.empty();
}

std::vector<CustomProvider> custom_providers(const ModelsYml& models) {
    std::vector<CustomProvider> out;
    for (auto& entry : models.providers()) {
        if (!is_custom(entry)) continue;
        CustomProvider p;
        if (entry.api_key && entry.api_key->kind == ApiKeySetting::Kind::EnvVar) p.api_key_env = entry.api_key->name;
        p.base_url = entry.base_url.value_or("");
        p.api = entry.api.value_or("");
        p.auth = entry.auth.value_or(AuthMode::ApiKey);
        for (auto& m : entry.models) p.models.push_back(std::move(m.id));
        p.id = std::move(entry.id);
        out.push_back(std::move(p));
    }
    return out;
}

std::expected<std::vector<Selector>, ValidationError> parse_selectors(std::string_view location,
                                                                      std::span<const std::string> entries) {
    std::vector<Selector> out;
    for (const std::string& entry : entries) {
        std::string_view rest = entry;
        while (true) {
            const auto comma = rest.find(',');
            const std::string_view s = rest.substr(0, comma);
            if (!detail::trim(s).empty()) {
                auto parsed = Selector::parse(s);
                if (!parsed) return std::unexpected(error(std::string(location), parsed.error()));
                out.push_back(std::move(*parsed));
            }
            if (comma == std::string_view::npos) break;
            rest.remove_prefix(comma + 1);
        }
    }
    return out;
}

std::vector<Step> apply_roles(OmpConfig& config, const std::vector<RoleChain>& before, const std::vector<RoleChain>& after) {
    std::vector<Step> results;
    auto find = [](const std::vector<RoleChain>& list, const std::string& role) -> const RoleChain* {
        auto it = std::find_if(list.begin(), list.end(), [&](const RoleChain& r) { return r.role == role; });
        return it == list.end() ? nullptr : &*it;
    };
    for (const RoleChain& old : before) {
        if (find(after, old.role)) continue;
        const std::string location = "Model roles › " + old.role;
        results.push_back(step(location, config.clear_model_role(old.role)));
        if (old.chain.size() > 1) {
            results.push_back(step(location, config.clear_fallback_chain(ChainKey::role(old.role))));
        }
    }
    for (const RoleChain& next : after) {
        const RoleChain* old = find(before, next.role);
        if (old && *old == next) continue;
        const std::string location = "Model roles › " + next.role;
        const ChainKey key = ChainKey::role(next.role);
        std::span<const std::string> old_fallbacks;
        if (old && !old->chain.empty()) old_fallbacks = std::span<const std::string>(old->chain).subspan(1);
        if (next.chain.empty()) {
            results.push_back(step(location, config.clear_model_role(next.role)));
            if (!old_fallbacks.empty()) results.push_back(step(location, config.clear_fallback_chain(key)));
            continue;
        }
        const std::string& primary = next.chain.front();
        if (!(old && !old->chain.empty() && old->chain.front() == primary)) {
            auto candidates = parse_selectors(location, std::span<const std::string>(&primary, 1));
            if (!candidates) {
                results.push_back(std::unexpected(std::move(candidates.error())));
            } else {
                results.push_back(step(location, config.set_model_role(next.role, *candidates)));
            }
        }
        const auto fallbacks = std::span<const std::string>(next.chain).subspan(1);
        if (!std::ranges::equal(fallbacks, old_fallbacks)) {
            if (fallbacks.empty()) {
                results.push_back(step(location, config.clear_fallback_chain(key)));
            } else {
                auto chain = parse_selectors(location, fallbacks);
                if (!chain) {
                    results.push_back(std::unexpected(std::move(chain.error())));
                } else {
                    results.push_back(step(location, config.set_fallback_chain(key, *chain)));
                }
            }
        }
    }
    return results;
}

Step apply_enabled_models(OmpConfig& config, const std::vector<ModelToggle>& toggles) {
    constexpr std::string_view LOCATION = "Enabled models";
    auto enabled = [](const ModelToggle& t) { return t.enabled; };
    if (std::none_of(toggles.begin(), toggles.end(), enabled)) {
        return std::unexpected(error(std::string(LOCATION), "at least one model must stay enabled"));
    }
    const auto entries = config.scoped_list(ScopedList::EnabledModels);
    std::vector<ScopedEntry> list;
    for (const ScopedEntry& e : entries) {
        if (!e.is_everywhere()) list.push_back(e);
    }
    if (!std::all_of(toggles.begin(), toggles.end(), enabled)) {
        auto matched = [](std::string_view pattern, const ModelToggle& toggle) {
            auto parts = ModelRef::split(toggle.selector);
            return parts && pattern_matches(pattern, parts->first, parts->second);
        };
        // Patterns that are not one of the listed selectors stay while every listed
        // model they select stays enabled.
        std::vector<std::string_view> kept;
        for (std::string_view p : everywhere(entries)) {
            const bool listed = std::any_of(toggles.begin(), toggles.end(), [&](const ModelToggle& t) { return t.selector == p; });
            const bool holds = std::all_of(toggles.begin(), toggles.end(),
                                           [&](const ModelToggle& t) { return !matched(p, t) || t.enabled; });
            if (!listed && holds) kept.push_back(p);
        }
        std::vector<ScopedEntry> next;
        for (std::string_view p : kept) next.push_back(ScopedEntry::everywhere(std::string(p)));
        for (const ModelToggle& t : toggles) {
            if (t.enabled && std::none_of(kept.begin(), kept.end(), [&](std::string_view p) { return matched(p, t); })) {
                next.push_back(ScopedEntry::everywhere(t.selector));
            }
        }
        for (ScopedEntry& e : list) next.push_back(std::move(e));
        list = std::move(next);
    }
    if (auto r = config.set_scoped_list(ScopedList::EnabledModels, list); !r) {
        return std::unexpected(error(std::string(LOCATION), r.error()));
    }
    return true;
}

Step apply_providers(OmpConfig& config, const std::vector<ProviderToggle>& before, const std::vector<ProviderToggle>& after) {
    constexpr std::string_view LOCATION = "Providers";
    auto ids = [](const std::vector<ProviderToggle>& list) {
        std::vector<std::string> out;
        for (const auto& p : list) out.push_back(p.id);
        return out;
    };
    bool changed = false;
    if (ids(before) != ids(after)) {
        if (auto r = config.set_provider_order(ids(after)); !r) {
            return std::unexpected(error(std::string(LOCATION), r.error()));
        }
        changed = true;
    }
    auto disabled = [](const std::vector<ProviderToggle>& list) {
        std::vector<std::string> out;
        for (const auto& p : list) {
            if (!p.enabled) out.push_back(p.id);
        }
        std::sort(out.begin(), out.end());
        return out;
    };
    if (disabled(after) != disabled(before)) {
        const auto entries = config.scoped_list(ScopedList::DisabledProviders);
        std::vector<ScopedEntry> list;
        for (const ScopedEntry& e : entries) {
            if (!e.is_everywhere() ||
                std::none_of(after.begin(), after.end(), [&](const ProviderToggle& p) { return p.id == e.value; })) {
                list.push_back(e);
            }
        }
        for (const auto& p : after) {
            if (!p.enabled) list.push_back(ScopedEntry::everywhere(p.id));
        }
        if (auto r = config.set_scoped_list(ScopedList::DisabledProviders, list); !r) {
            return std::unexpected(error(std::string(LOCATION), r.error()));
        }
        changed = true;
    }
    return changed;
}

std::expected<models_yml::OverrideField, std::string> override_field(const std::string& key, const FieldValue& value) {
    using F = models_yml::OverrideField;
    auto it = std::find_if(OVERRIDE_FIELDS.begin(), OVERRIDE_FIELDS.end(), [&](const auto& f) { return f.first == key; });
    if (it == OVERRIDE_FIELDS.end()) {
        std::string names;
        for (const auto& [name, _] : OVERRIDE_FIELDS) {
            if (!names.empty()) names += ", ";
            names += name;
        }
        return std::unexpected("`" + key + "` is not an override field this screen edits (" + names + ")");
    }
    auto text = [&]() -> std::expected<std::string, std::string> {
        if (const auto* s = value.as_text()) return *s;
        return std::unexpected("`" + key + "` is text");
    };
    auto count = [&]() -> std::expected<std::uint64_t, std::string> {
        if (const auto* n = value.as_int(); n && *n > 0) return static_cast<std::uint64_t>(*n);
        return std::unexpected("`" + key + "` is a whole number above zero");
    };
    auto flag = [&]() -> std::expected<bool, std::string> {
        if (const auto* b = value.as_bool()) return *b;
        return std::unexpected("`" + key + "` is yes or no");
    };
    switch (it->second) {
    case OverrideKey::Name: return text().transform(F::name);
    case OverrideKey::ContextWindow: return count().transform(F::context_window);
    case OverrideKey::MaxContextWindow: return count().transform(F::max_context_window);
    case OverrideKey::MaxTokens: return count().transform(F::max_tokens);
    case OverrideKey::Reasoning: return flag().transform(F::reasoning);
    case OverrideKey::ContextPromotionTarget: return text().transform(F::context_promotion_target);
    case OverrideKey::CompactionModel: return text().transform(F::compaction_model);
    case OverrideKey::PreferWebsockets: return flag().transform(F::prefer_websockets);
    case OverrideKey::Input:
    case OverrideKey::Cost: break; // not listed in OVERRIDE_FIELDS
    }
    std::unreachable();
}

std::vector<Step> apply_overrides(ModelsYml& models, const std::vector<ModelOverride>& before,
                                  const std::vector<ModelOverride>& after) {
    using Fields = std::map<std::string, std::vector<OverrideField>>;
    auto fields = [](const std::vector<ModelOverride>& list) {
        Fields out;
        for (const auto& o : list) out[o.selector] = o.fields;
        return out;
    };
    const Fields old = fields(before);
    const Fields next = fields(after);
    std::vector<std::string> selectors;
    for (const auto& [s, _] : old) selectors.push_back(s);
    for (const auto& [s, _] : next) {
        if (!old.contains(s)) selectors.push_back(s);
    }
    std::vector<Step> results;
    for (const std::string& selector : selectors) {
        auto o = old.find(selector);
        auto n = next.find(selector);
        const std::vector<OverrideField>* old_fields = o == old.end() ? nullptr : &o->second;
        const std::vector<OverrideField>* new_fields = n == next.end() ? nullptr : &n->second;
        if ((!old_fields && !new_fields) || (old_fields && new_fields && *old_fields == *new_fields)) continue;
        const std::string location = "Model overrides › " + selector;
        auto parts = ModelRef::split(selector);
        if (!parts) {
            results.push_back(std::unexpected(error(location, "write the model as provider/model")));
            continue;
        }
        const auto [provider, model] = *parts;
        if (old_fields) {
            for (const OverrideField& field : *old_fields) {
                const bool gone = !new_fields || std::none_of(new_fields->begin(), new_fields->end(),
                                                              [&](const OverrideField& f) { return f.key == field.key; });
                if (!gone) continue;
                auto it = std::find_if(OVERRIDE_FIELDS.begin(), OVERRIDE_FIELDS.end(),
                                       [&](const auto& f) { return f.first == field.key; });
                if (it != OVERRIDE_FIELDS.end()) {
                    results.push_back(step(location, models.clear_override(provider, model, it->second)));
                }
            }
        }
        if (new_fields) {
            for (const OverrideField& field : *new_fields) {
                if (old_fields && std::find(old_fields->begin(), old_fields->end(), field) != old_fields->end()) continue;
                auto f = override_field(field.key, field.value);
                if (!f) {
                    results.push_back(std::unexpected(error(location, std::move(f.error()))));
                } else {
                    results.push_back(step(location, models.set_override(provider, model, *f)));
                }
            }
        }
    }
    return results;
}

std::expected<void, std::string> write_custom_provider(ModelsYml& models, const CustomProvider* old, const CustomProvider& next) {
    const std::optional<models_yml::ProviderEntry> existing = old ? models.provider(next.id) : std::nullopt;
    models_yml::CustomProvider provider;
    if (existing) {
        provider = models_yml::CustomProvider{
            .id = existing->id,
            .base_url = existing->base_url,
            .api = existing->api,
            .api_key = ApiKeyWrite::keep(),
            .auth = existing->auth,
            .auth_header = existing->auth_header,
            .discovery = existing->discovery,
            .models = existing->models,
        };
    } else {
        provider.id = next.id;
    }
    auto changed = [&](std::string CustomProvider::* f) { return !old || old->*f != next.*f; };
    auto optional = [](const std::string& s) -> std::optional<std::string> {
        if (s.empty()) return std::nullopt;
        return s;
    };
    if (changed(&CustomProvider::base_url)) provider.base_url = optional(next.base_url);
    if (changed(&CustomProvider::api)) provider.api = optional(next.api);
    if (changed(&CustomProvider::api_key_env)) {
        if (next.api_key_env.empty()) {
            provider.api_key = ApiKeyWrite::remove();
        } else {
            auto key = KeyReference::env_var(next.api_key_env);
            if (!key) return std::unexpected(key.error().to_string());
            provider.api_key = ApiKeyWrite::set(std::move(*key));
        }
    }
    if (!old || old->auth != next.auth) {
        // A new provider on the default (API key) auth leaves `auth` out, as omp does.
        provider.auth = (old || next.auth != AuthMode::ApiKey) ? std::optional<AuthMode>(next.auth) : std::nullopt;
    }
    if (!old || old->models != next.models) {
        const std::vector<models_yml::CustomModel> kept = existing ? existing->models : std::vector<models_yml::CustomModel>{};
        provider.models.clear();
        for (const std::string& id : next.models) {
            auto it = std::find_if(kept.begin(), kept.end(), [&](const auto& m) { return m.id == id; });
            provider.models.push_back(it != kept.end() ? *it : models_yml::CustomModel{.id = id});
        }
    }
    if (auto r = models.set_custom_provider(provider); !r) return std::unexpected(r.error().to_string());
    return {};
}

std::vector<Step> apply_custom_providers(ModelsYml& models, const std::vector<CustomProvider>& before,
                                         const std::vector<CustomProvider>& after) {
    auto find = [](const std::vector<CustomProvider>& list, const std::string& id) -> const CustomProvider* {
        auto it = std::find_if(list.begin(), list.end(), [&](const CustomProvider& p) { return p.id == id; });
        return it == list.end() ? nullptr : &*it;
    };
    std::vector<Step> results;
    for (const CustomProvider& old : before) {
        if (find(after, old.id)) continue;
        results.push_back(step("Custom providers › " + old.id, models.remove_provider(old.id)));
    }
    for (const CustomProvider& next : after) {
        const CustomProvider* old = find(before, next.id);
        if (old && *old == next) continue;
        const std::string location = "Custom providers › " + next.id;
        if (auto r = write_custom_provider(models, old, next); !r) {
            results.push_back(std::unexpected(error(location, std::move(r.error()))));
        } else {
            results.push_back(true);
        }
    }
    return results;
}

} // namespace

bool glob_match(std::string_view pattern, std::string_view text) noexcept {
    if (pattern.empty()) return text.empty();
    if (pattern.front() == '*') {
        return glob_match(pattern.substr(1), text) || (!text.empty() && glob_match(pattern, text.substr(1)));
    }
    if (text.empty()) return false;
    if (pattern.front() == '?' || pattern.front() == text.front()) return glob_match(pattern.substr(1), text.substr(1));
    return false;
}

bool pattern_matches(std::string_view pattern, std::string_view provider, std::string_view id) {
    std::string selector(provider);
    selector += '/';
    selector += id;
    return pattern == selector || pattern == id || glob_match(pattern, selector) ||
           (pattern.find('/') == std::string_view::npos && glob_match(pattern, id));
}

std::expected<ModelSettings, std::string> settings(const OmpConfig& config, const ModelsYml& models,
                                                   std::span<const rpc::ModelInfo> available,
                                                   const std::unordered_map<std::string, std::string>& provider_names) {
    auto role_chains = roles(config);
    if (!role_chains) return std::unexpected(std::move(role_chains.error()));
    return ModelSettings{
        .roles = std::move(*role_chains),
        .models = model_toggles(config, available),
        .providers = provider_toggles(config, available, provider_names),
        .overrides = overrides(models),
        .custom_providers = custom_providers(models),
        // Filled by the backend, which owns the harness configs.
        .harnesses = {},
    };
}

std::expected<Edited, std::vector<ValidationError>> apply_settings(OmpConfig& config, ModelsYml& models,
                                                                   const ModelSettings& before, const ModelSettings& after) {
    std::vector<ValidationError> errors;
    Edited edited;
    auto track = [&](Step result, bool& file) {
        if (result) {
            file |= *result;
        } else {
            errors.push_back(std::move(result.error()));
        }
    };
    for (Step& r : apply_roles(config, before.roles, after.roles)) track(std::move(r), edited.config);
    if (before.models != after.models) track(apply_enabled_models(config, after.models), edited.config);
    if (before.providers != after.providers) track(apply_providers(config, before.providers, after.providers), edited.config);
    for (Step& r : apply_custom_providers(models, before.custom_providers, after.custom_providers)) {
        track(std::move(r), edited.models);
    }
    for (Step& r : apply_overrides(models, before.overrides, after.overrides)) track(std::move(r), edited.models);
    if (errors.empty()) return edited;
    return std::unexpected(std::move(errors));
}

} // namespace loa::settings::models::settings
