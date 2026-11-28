#include "loa/app/view/settings/models.hpp"

#include <type_traits>
#include <utility>
#include <variant>

namespace loa::app::view::settings {

namespace {

namespace selector = ::loa::settings::models::selector;
namespace login = ::loa::settings::models::login;
namespace domain = ::loa::settings::models::settings;
namespace shown = ::loa::view::settings::models;

template <class... Fs>
struct Overloaded : Fs... {
    using Fs::operator()...;
};

shown::AuthMode auth_mode_to_view(domain::AuthMode mode) {
    switch (mode) {
    case domain::AuthMode::ApiKey: return shown::AuthMode::ApiKey;
    case domain::AuthMode::None: return shown::AuthMode::None;
    case domain::AuthMode::OAuth: return shown::AuthMode::OAuth;
    }
    return shown::AuthMode::ApiKey;
}

domain::AuthMode auth_mode_from_view(shown::AuthMode mode) {
    switch (mode) {
    case shown::AuthMode::ApiKey: return domain::AuthMode::ApiKey;
    case shown::AuthMode::None: return domain::AuthMode::None;
    case shown::AuthMode::OAuth: return domain::AuthMode::OAuth;
    }
    return domain::AuthMode::ApiKey;
}

// Both sides hold Text, Int, Float and Bool in the same order.
shown::FieldValue field_value_to_view(const domain::FieldValue& value) {
    return std::visit([](const auto& v) { return shown::FieldValue(v); }, value.v);
}

domain::FieldValue field_value_from_view(shown::FieldValue value) {
    return std::visit([](auto&& v) { return domain::FieldValue{domain::FieldValue::Storage(std::move(v))}; }, std::move(value));
}

} // namespace

shown::ModelRef model_ref_to_view(const selector::ModelRef& model) { return shown::ModelRef{model.provider, model.id}; }

selector::ModelRef model_ref_from_view(shown::ModelRef model) { return selector::ModelRef{std::move(model.provider), std::move(model.id)}; }

shown::Catalog catalog_to_view(const selector::Catalog& catalog) {
    shown::Catalog out;
    out.models.reserve(catalog.models.size());
    for (const auto& m : catalog.models) {
        out.models.push_back(shown::ModelInfo{model_ref_to_view(m.model), m.name, m.provider_name});
    }
    out.roles.reserve(catalog.roles.size());
    for (const auto& r : catalog.roles) out.roles.push_back(shown::RoleAssignment{r.role, model_ref_to_view(r.model)});
    return out;
}

shown::ProviderKey provider_key_to_view(const login::ProviderKey& key) { return shown::ProviderKey{key.harness, key.provider}; }

login::ProviderKey provider_key_from_view(shown::ProviderKey key) { return login::ProviderKey{std::move(key.harness), std::move(key.provider)}; }

shown::ProviderEntry provider_to_view(const login::ProviderEntry& entry) {
    shown::Auth auth = std::visit(Overloaded{
                                      [](const login::auth::Login& a) -> shown::Auth { return shown::auth::Login{a.signed_in, a.account}; },
                                      [](const login::auth::ApiKey& a) -> shown::Auth { return shown::auth::ApiKey{a.configured_via, a.instructions}; },
                                      [](const login::auth::Unavailable& a) -> shown::Auth { return shown::auth::Unavailable{a.reason}; },
                                  },
                                  entry.auth);
    return shown::ProviderEntry{provider_key_to_view(entry.key), entry.name, entry.harness_name, std::move(auth)};
}

shown::Credential credential_to_view(const login::Credential& credential) {
    return shown::Credential{credential.id, credential.label, credential.detail, credential.active};
}

shown::LoginUpdate login_update_to_view(::loa::contracts::LoginUpdate update) {
    namespace c = ::loa::contracts::login;
    namespace v = shown::login_update;
    return std::visit(Overloaded{
                          [](c::Url& u) -> shown::LoginUpdate { return v::Url{std::move(u.url), std::move(u.launch_url), std::move(u.instructions)}; },
                          [](c::Progress& p) -> shown::LoginUpdate { return v::Progress{std::move(p.text)}; },
                          [](c::Prompt& p) -> shown::LoginUpdate { return v::Prompt{std::move(p.message), std::move(p.placeholder)}; },
                          [](c::Succeeded& s) -> shown::LoginUpdate { return v::Succeeded{std::move(s.account)}; },
                          [](c::Failed& f) -> shown::LoginUpdate { return v::Failed{std::move(f.reason)}; },
                          [](c::Cancelled&) -> shown::LoginUpdate { return v::Cancelled{}; },
                      },
                      update);
}

shown::ValidationError validation_error_to_view(const domain::ValidationError& error) { return shown::ValidationError{error.location, error.message}; }

shown::ModelSettings settings_to_view(const domain::ModelSettings& s) {
    shown::ModelSettings out;
    for (const auto& r : s.roles) out.roles.push_back(shown::RoleChain{r.role, r.chain});
    for (const auto& m : s.models) out.models.push_back(shown::ModelToggle{m.selector, m.enabled});
    for (const auto& p : s.providers) out.providers.push_back(shown::ProviderToggle{p.id, p.name, p.enabled});
    for (const auto& o : s.overrides) {
        shown::ModelOverride shown_override{o.selector, {}};
        for (const auto& f : o.fields) shown_override.fields.push_back(shown::OverrideField{f.key, field_value_to_view(f.value)});
        out.overrides.push_back(std::move(shown_override));
    }
    for (const auto& p : s.custom_providers) {
        out.custom_providers.push_back(shown::CustomProvider{p.id, p.base_url, p.api, auth_mode_to_view(p.auth), p.api_key_env, p.models});
    }
    for (const auto& h : s.harnesses) out.harnesses.push_back(shown::HarnessModel{h.harness, h.name, h.config_path, h.model, h.effort, h.efforts});
    return out;
}

domain::ModelSettings settings_from_view(shown::ModelSettings s) {
    domain::ModelSettings out;
    for (auto& r : s.roles) out.roles.push_back(domain::RoleChain{std::move(r.role), std::move(r.chain)});
    for (auto& m : s.models) out.models.push_back(domain::ModelToggle{std::move(m.selector), m.enabled});
    for (auto& p : s.providers) out.providers.push_back(domain::ProviderToggle{std::move(p.id), std::move(p.name), p.enabled});
    for (auto& o : s.overrides) {
        domain::ModelOverride model_override{std::move(o.selector), {}};
        for (auto& f : o.fields) model_override.fields.push_back(domain::OverrideField{std::move(f.key), field_value_from_view(std::move(f.value))});
        out.overrides.push_back(std::move(model_override));
    }
    for (auto& p : s.custom_providers) {
        out.custom_providers.push_back(domain::CustomProvider{std::move(p.id), std::move(p.base_url), std::move(p.api), auth_mode_from_view(p.auth),
                                                              std::move(p.api_key_env), std::move(p.models)});
    }
    for (auto& h : s.harnesses) {
        out.harnesses.push_back(domain::HarnessModel{std::move(h.harness), std::move(h.name), std::move(h.config_path), std::move(h.model),
                                                     std::move(h.effort), std::move(h.efforts)});
    }
    return out;
}

} // namespace loa::app::view::settings
