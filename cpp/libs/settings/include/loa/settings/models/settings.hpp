#pragma once

// Conversions between omp's settings files and catalog and the plain data the screens
// show (port of `models/settings.rs`).
//
// Reading builds `ModelSettings` from `config.yml`, `models.yml` and the available
// models. Writing is a diff: `apply_settings` compares what the screen started from
// with what it saves and edits only the keys that changed, so settings the screen does
// not show (path scoped entries, globs, secret key commands, model metadata, headers)
// stay as they are.

#include <array>
#include <cstdint>
#include <expected>
#include <span>
#include <string>
#include <string_view>
#include <unordered_map>
#include <utility>
#include <variant>
#include <vector>

#include "loa/settings/models/config.hpp"
#include "loa/settings/models/error.hpp"
#include "loa/settings/models/models_yml.hpp"
#include "loa/settings/models/rpc.hpp"

namespace loa::settings::models::settings {

using models::Error;
using models::Result;
using models::SettingsValidator;
using models_yml::AuthMode;
using models_yml::is_env_var_name;
using models_yml::is_provider_id;

// The default model and effort another harness (Claude Code, Codex) keeps in its own
// config file. Empty values leave the choice to the harness.
struct HarnessModel {
    // Harness key, e.g. `claude`.
    std::string harness;
    // Display name, e.g. "Claude Code".
    std::string name;
    // The config file the values live in, for display.
    std::string config_path;
    std::string model;
    std::string effort;
    // Effort values the harness accepts in its config.
    std::vector<std::string> efforts;
    friend bool operator==(const HarnessModel&, const HarnessModel&) = default;
};

// A model role and its ordered chain: `chain[0]` is the primary model
// (`modelRoles.<role>`), the rest are its fallbacks (`retry.fallbackChains.<role>`).
struct RoleChain {
    std::string role;
    std::vector<std::string> chain;
    friend bool operator==(const RoleChain&, const RoleChain&) = default;
};

// Whether a model selector is listed in `enabledModels`.
struct ModelToggle {
    std::string selector;
    bool enabled = false;
    friend bool operator==(const ModelToggle&, const ModelToggle&) = default;
};

// A provider and whether it is enabled. In provider order.
struct ProviderToggle {
    std::string id;
    std::string name;
    bool enabled = false;
    friend bool operator==(const ProviderToggle&, const ProviderToggle&) = default;
};

// A typed override value.
struct FieldValue {
    using Storage = std::variant<std::string, std::int64_t, double, bool>;
    Storage v;

    [[nodiscard]] static FieldValue text(std::string s) { return {std::move(s)}; }
    [[nodiscard]] static FieldValue integer(std::int64_t n) { return {n}; }
    [[nodiscard]] static FieldValue real(double f) { return {f}; }
    [[nodiscard]] static FieldValue boolean(bool b) { return {b}; }

    [[nodiscard]] const std::string* as_text() const noexcept { return std::get_if<std::string>(&v); }
    [[nodiscard]] const std::int64_t* as_int() const noexcept { return std::get_if<std::int64_t>(&v); }
    [[nodiscard]] const double* as_float() const noexcept { return std::get_if<double>(&v); }
    [[nodiscard]] const bool* as_bool() const noexcept { return std::get_if<bool>(&v); }

    friend bool operator==(const FieldValue&, const FieldValue&) = default;
};

// One overridden model field, e.g. `contextWindow: 200000`.
struct OverrideField {
    std::string key;
    FieldValue value;
    friend bool operator==(const OverrideField&, const OverrideField&) = default;
};

// The `modelOverrides` fields set for one model selector.
struct ModelOverride {
    std::string selector;
    std::vector<OverrideField> fields;
    friend bool operator==(const ModelOverride&, const ModelOverride&) = default;
};

// A provider defined in models.yml. The API key is only ever the name of an environment
// variable that holds it; the app never handles the key itself.
struct CustomProvider {
    std::string id;
    std::string base_url;
    std::string api;
    AuthMode auth = AuthMode::ApiKey;
    std::string api_key_env;
    std::vector<std::string> models;
    friend bool operator==(const CustomProvider&, const CustomProvider&) = default;
};

// Everything the screen edits, as the app reads it from omp's config files.
struct ModelSettings {
    std::vector<RoleChain> roles;
    std::vector<ModelToggle> models;
    std::vector<ProviderToggle> providers;
    std::vector<ModelOverride> overrides;
    std::vector<CustomProvider> custom_providers;
    // The other harnesses' own default model settings.
    std::vector<HarnessModel> harnesses;
    friend bool operator==(const ModelSettings&, const ModelSettings&) = default;
};

// An error the app's validation (or omp) reported for a saved settings value.
struct ValidationError {
    std::string location;
    std::string message;
    friend bool operator==(const ValidationError&, const ValidationError&) = default;
};

// The override fields the screen edits, with the type each is edited as.
inline constexpr std::array<std::pair<std::string_view, models_yml::OverrideKey>, 8> OVERRIDE_FIELDS = {{
    {"name", models_yml::OverrideKey::Name},
    {"contextWindow", models_yml::OverrideKey::ContextWindow},
    {"maxContextWindow", models_yml::OverrideKey::MaxContextWindow},
    {"maxTokens", models_yml::OverrideKey::MaxTokens},
    {"reasoning", models_yml::OverrideKey::Reasoning},
    {"contextPromotionTarget", models_yml::OverrideKey::ContextPromotionTarget},
    {"compactionModel", models_yml::OverrideKey::CompactionModel},
    {"preferWebsockets", models_yml::OverrideKey::PreferWebsockets},
}};

// `*` and `?` glob over bytes.
[[nodiscard]] bool glob_match(std::string_view pattern, std::string_view text) noexcept;

// Whether `pattern` (an `enabledModels` entry: exact selector, bare id or glob) selects
// the model `provider/id`.
[[nodiscard]] bool pattern_matches(std::string_view pattern, std::string_view provider, std::string_view id);

// Reads the settings the screen shows.
[[nodiscard]] std::expected<ModelSettings, std::string> settings(
    const config::OmpConfig& config, const models_yml::ModelsYml& models, std::span<const rpc::ModelInfo> available,
    const std::unordered_map<std::string, std::string>& provider_names);

// Which files `apply_settings` edited.
struct Edited {
    bool config = false;
    bool models = false;
    friend bool operator==(const Edited&, const Edited&) = default;
};

// Applies the difference between `before` (what the screen loaded) and `after` (what it
// saves) to the loaded files. Nothing is written to disk.
[[nodiscard]] std::expected<Edited, std::vector<ValidationError>> apply_settings(config::OmpConfig& config,
                                                                                models_yml::ModelsYml& models,
                                                                                const ModelSettings& before,
                                                                                const ModelSettings& after);

} // namespace loa::settings::models::settings
