#pragma once

// Reports credential *sources* without exposing their values (port of `models/keys.rs`).
// Omp owns all credential resolution and login storage; this module never handles keys.

#include <cstdint>
#include <span>
#include <string>
#include <string_view>

#include "loa/settings/models/models_yml.hpp"

namespace loa::settings::models::keys {

// A source omp is configured to consult for a provider key.
struct KeySource {
    enum class Kind : std::uint8_t {
        ConfigEnvironment, // `models.yml` names an environment variable (`name`)
        SecretCommand,     // `models.yml` runs a secret command; its text and output stay private
        ConfigEntry,       // `models.yml` contains an inline value, which is never returned
        Environment,       // omp's built-in provider variable `name` is present
        NotConfigured,     // no configured source detected; stored omp credentials are not inspected
    };
    Kind kind = Kind::NotConfigured;
    std::string name;

    [[nodiscard]] static KeySource config_environment(std::string name) { return {Kind::ConfigEnvironment, std::move(name)}; }
    [[nodiscard]] static KeySource secret_command() { return {Kind::SecretCommand, {}}; }
    [[nodiscard]] static KeySource config_entry() { return {Kind::ConfigEntry, {}}; }
    [[nodiscard]] static KeySource environment(std::string name) { return {Kind::Environment, std::move(name)}; }
    [[nodiscard]] static KeySource not_configured() { return {}; }

    friend bool operator==(const KeySource&, const KeySource&) = default;
};

// Checks presence only, never yields the environment variable's contents.
class EnvironmentPresence {
  public:
    virtual ~EnvironmentPresence() = default;
    [[nodiscard]] virtual bool contains(std::string_view name) const = 0;
};

// Presence from the current process environment; no values leave this method.
class ProcessEnvironment final : public EnvironmentPresence {
  public:
    [[nodiscard]] bool contains(std::string_view name) const override;
};

// Built-in credential environment names come from `builtin_environment_names`; custom
// providers can pass their own variable names in `environment_names`. A provider's OAuth
// or key stored in omp's agent.db is not inspected; `get_login_providers` reports
// signed-in OAuth state.
[[nodiscard]] KeySource credential_source(const models_yml::ModelsYml& models, std::string_view provider,
                                          std::span<const std::string_view> environment_names, const EnvironmentPresence& env);

// Human-readable setup directions for the selected source, without a key.
[[nodiscard]] std::string key_instructions(std::string_view provider, const KeySource& source,
                                           std::span<const std::string_view> environment_names);

// Known provider key names, in omp's credential fallback order. Empty means the provider
// uses its own login or has no ordinary API key.
[[nodiscard]] std::span<const std::string_view> builtin_environment_names(std::string_view provider) noexcept;

} // namespace loa::settings::models::keys
