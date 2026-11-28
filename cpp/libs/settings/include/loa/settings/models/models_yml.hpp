#pragma once

// Typed editor for omp's `models.yml`: custom providers and per model overrides
// (port of `models/models_yml.rs`).
//
// Shape, as omp's `models.md` defines it:
//
//   providers:
//     <provider-id>:
//       baseUrl: https://api.example.com/v1
//       api: openai-completions
//       apiKey: MY_PROVIDER_API_KEY   # env var name, or "!command"
//       auth: apiKey | none | oauth
//       discovery: { type: ollama }
//       modelOverrides:
//         <model-id>: { name: ..., contextWindow: ... }
//       models:
//         - id: ...
//
// Provider keys this editor does not model (`headers`, `compat`, ...) are left untouched.
//
// API keys: `apiKey` can only be written as a `KeyReference` (an environment variable
// name or a `!command`). An inline key found in the file is reported as
// `ApiKeySetting::Kind::Literal`, which carries no value.

#include <array>
#include <cstdint>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/settings/models/error.hpp"
#include "loa/settings/models/rpc.hpp"
#include "loa/settings/models/yaml.hpp"

namespace loa::settings::models::models_yml {

using rpc::ModelCost;

// The `api` values omp accepts for providers and models.
inline constexpr std::array<std::string_view, 14> APIS = {
    "openai-completions",   "openai-responses",     "openai-codex-responses", "azure-openai-responses",
    "anthropic-messages",   "bedrock-converse-stream", "google-generative-ai", "google-gemini-cli",
    "google-vertex",        "typesafe",             "openrouter-decisions",   "openai-decisions",
    "google-decisions",     "workers-ai-systemone",
};

// The `discovery.type` values omp accepts.
inline constexpr std::array<std::string_view, 7> DISCOVERY_TYPES = {
    "ollama", "llama.cpp", "lm-studio", "openai-models-list", "proxy", "litellm", "apple-foundation-models",
};

// How a provider authenticates (`auth`). Defaults to `ApiKey`.
enum class AuthMode : std::uint8_t { ApiKey, None, OAuth };
[[nodiscard]] std::string_view as_str(AuthMode m) noexcept;
[[nodiscard]] std::optional<AuthMode> parse_auth_mode(std::string_view s) noexcept;

// Where omp reads a provider's key from. There is deliberately no kind that holds a
// key value.
struct KeyReference {
    enum class Kind : std::uint8_t {
        EnvVar,  // an environment variable name, written as is
        Command, // a shell command whose trimmed stdout is the key, written `!command`
    };
    Kind kind = Kind::EnvVar;
    std::string value;

    // A reference to environment variable `name` (upper case letters, digits and `_`,
    // not starting with a digit). The upper case rule keeps a pasted key from being
    // stored as if it were a variable name.
    [[nodiscard]] static Result<KeyReference> env_var(std::string_view name);
    // A reference to a command that prints the key, such as `op read op://dev/openai/api-key`.
    [[nodiscard]] static Result<KeyReference> command(std::string_view command);

    friend bool operator==(const KeyReference&, const KeyReference&) = default;
};

// Whether `name` looks like an environment variable name by convention.
[[nodiscard]] bool is_env_var_name(std::string_view name) noexcept;

// What a provider's `apiKey` holds, as reported to the app.
struct ApiKeySetting {
    enum class Kind : std::uint8_t {
        EnvVar,  // names an environment variable (`name`)
        Command, // runs a secret command; command text is never exposed
        Literal, // an inline value; its content is never reported
    };
    Kind kind = Kind::Literal;
    std::string name;

    [[nodiscard]] static ApiKeySetting env_var(std::string name) { return {Kind::EnvVar, std::move(name)}; }
    [[nodiscard]] static ApiKeySetting command() { return {Kind::Command, {}}; }
    [[nodiscard]] static ApiKeySetting literal() { return {Kind::Literal, {}}; }

    friend bool operator==(const ApiKeySetting&, const ApiKeySetting&) = default;
};

// An input modality a model accepts.
enum class Modality : std::uint8_t { Text, Image };
[[nodiscard]] std::string_view as_str(Modality m) noexcept;
[[nodiscard]] std::optional<Modality> parse_modality(std::string_view s) noexcept;

// A model defined by a custom provider (`providers.<id>.models[]`).
struct CustomModel {
    std::string id;
    std::optional<std::string> name;
    std::optional<std::string> api;
    std::optional<bool> reasoning;
    std::optional<std::vector<Modality>> input;
    std::optional<std::uint64_t> context_window;
    std::optional<std::uint64_t> max_tokens;
    std::optional<ModelCost> cost;
    friend bool operator==(const CustomModel&, const CustomModel&) = default;
};

// What `ModelsYml::set_custom_provider` does with the provider's `apiKey`.
struct ApiKeyWrite {
    enum class Kind : std::uint8_t {
        Remove, // removes `apiKey`
        Keep,   // leaves whatever `apiKey` holds, so an edit of other fields never needs
                // (or exposes) a secret command or inline key already in the file
        Set,    // writes `key`
    };
    Kind kind = Kind::Remove;
    KeyReference key;

    [[nodiscard]] static ApiKeyWrite remove() { return {}; }
    [[nodiscard]] static ApiKeyWrite keep() { return {Kind::Keep, {}}; }
    [[nodiscard]] static ApiKeyWrite set(KeyReference key) { return {Kind::Set, std::move(key)}; }

    friend bool operator==(const ApiKeyWrite&, const ApiKeyWrite&) = default;
};

// The fields of a custom provider this editor writes. Empty optionals remove the key.
struct CustomProvider {
    std::string id;
    std::optional<std::string> base_url;
    std::optional<std::string> api;
    ApiKeyWrite api_key;
    std::optional<AuthMode> auth;
    // Send the resolved key as `Authorization: Bearer`.
    std::optional<bool> auth_header;
    std::optional<std::string> discovery;
    std::vector<CustomModel> models;
    friend bool operator==(const CustomProvider&, const CustomProvider&) = default;
};

// A per model override as read from the file. Keys the editor does not model are
// listed in `other` so the screen can say they exist.
struct ModelOverride {
    std::optional<std::string> name;
    std::optional<std::uint64_t> context_window;
    std::optional<std::uint64_t> max_context_window;
    std::optional<std::uint64_t> max_tokens;
    std::optional<bool> reasoning;
    std::optional<std::vector<Modality>> input;
    std::optional<std::string> context_promotion_target;
    std::optional<std::string> compaction_model;
    std::optional<bool> prefer_websockets;
    std::optional<ModelCost> cost;
    std::vector<std::string> other;
    friend bool operator==(const ModelOverride&, const ModelOverride&) = default;
};

// A provider entry as read from the file.
struct ProviderEntry {
    std::string id;
    std::optional<std::string> base_url;
    std::optional<std::string> api;
    std::optional<ApiKeySetting> api_key;
    std::optional<AuthMode> auth;
    std::optional<bool> auth_header;
    std::optional<std::string> discovery;
    std::vector<CustomModel> models;
    std::vector<std::pair<std::string, ModelOverride>> overrides;
    friend bool operator==(const ProviderEntry&, const ProviderEntry&) = default;
};

// Names an override field, for clearing.
enum class OverrideKey : std::uint8_t {
    Name,
    ContextWindow,
    MaxContextWindow,
    MaxTokens,
    Reasoning,
    Input,
    ContextPromotionTarget,
    CompactionModel,
    PreferWebsockets,
    Cost,
};
[[nodiscard]] std::string_view as_str(OverrideKey k) noexcept;
[[nodiscard]] std::optional<OverrideKey> parse_override_key(std::string_view s) noexcept;

// One override field with its value, for writing. The value's type follows the key:
// text (Name, ContextPromotionTarget, CompactionModel), count (ContextWindow,
// MaxContextWindow, MaxTokens), flag (Reasoning, PreferWebsockets), modalities (Input)
// or cost (Cost).
class OverrideField {
  public:
    using Value = std::variant<std::string, std::uint64_t, bool, std::vector<Modality>, ModelCost>;

    [[nodiscard]] static OverrideField name(std::string v) { return {OverrideKey::Name, std::move(v)}; }
    [[nodiscard]] static OverrideField context_window(std::uint64_t v) { return {OverrideKey::ContextWindow, v}; }
    [[nodiscard]] static OverrideField max_context_window(std::uint64_t v) { return {OverrideKey::MaxContextWindow, v}; }
    [[nodiscard]] static OverrideField max_tokens(std::uint64_t v) { return {OverrideKey::MaxTokens, v}; }
    [[nodiscard]] static OverrideField reasoning(bool v) { return {OverrideKey::Reasoning, v}; }
    [[nodiscard]] static OverrideField input(std::vector<Modality> v) { return {OverrideKey::Input, std::move(v)}; }
    [[nodiscard]] static OverrideField context_promotion_target(std::string v) {
        return {OverrideKey::ContextPromotionTarget, std::move(v)};
    }
    [[nodiscard]] static OverrideField compaction_model(std::string v) { return {OverrideKey::CompactionModel, std::move(v)}; }
    [[nodiscard]] static OverrideField prefer_websockets(bool v) { return {OverrideKey::PreferWebsockets, v}; }
    [[nodiscard]] static OverrideField cost(ModelCost v) { return {OverrideKey::Cost, v}; }

    // The field's key.
    [[nodiscard]] OverrideKey key() const noexcept { return key_; }
    [[nodiscard]] const Value& value() const noexcept { return value_; }
    // The value as YAML, validated.
    [[nodiscard]] Result<yaml::Yaml> yaml() const;

    friend bool operator==(const OverrideField&, const OverrideField&) = default;

  private:
    OverrideField(OverrideKey key, Value value) : key_(key), value_(std::move(value)) {}

    OverrideKey key_;
    Value value_;
};

// Whether `id` is a provider id omp accepts: letters, digits, `-`, `_` or `.`.
[[nodiscard]] bool is_provider_id(std::string_view id) noexcept;

// omp's `models.yml`, loaded for editing.
class ModelsYml {
  public:
    // Loads `path`; a missing file is an empty document.
    [[nodiscard]] static Result<ModelsYml> load(const std::filesystem::path& path);

    // The edited document text.
    [[nodiscard]] const std::string& text() const noexcept { return doc_.text(); }
    // The file this document was loaded from.
    [[nodiscard]] const std::filesystem::path& path() const noexcept { return snapshot_.path(); }

    // Writes the edits atomically.
    [[nodiscard]] Result<> save();
    // Asks `validator` (omp itself) to load the edited file, then saves. The document
    // must outlive the coroutine.
    [[nodiscard]] contracts::Awaitable<Result<>> save_validated(const SettingsValidator& validator);
    // Asks `validator` (omp itself) to load the edited file. Writes nothing.
    [[nodiscard]] contracts::Awaitable<Result<>> validate(const SettingsValidator& validator) const;

    // Every provider entry, in file order.
    [[nodiscard]] std::vector<ProviderEntry> providers() const;
    // One provider entry.
    [[nodiscard]] std::optional<ProviderEntry> provider(std::string_view id) const;

    // Creates or updates a custom provider. Only the keys of `CustomProvider` change;
    // `headers`, `compat`, `modelOverrides` and other keys stay.
    [[nodiscard]] Result<> set_custom_provider(const CustomProvider& p);
    // Removes a provider entry entirely.
    [[nodiscard]] Result<bool> remove_provider(std::string_view id);
    // Sets one override field for `model` under `provider` (a built in or custom
    // provider id).
    [[nodiscard]] Result<> set_override(std::string_view provider, std::string_view model, const OverrideField& field);
    // Removes one override field. An override left empty is removed, and a provider
    // left with nothing is removed too.
    [[nodiscard]] Result<bool> clear_override(std::string_view provider, std::string_view model, OverrideKey key);

  private:
    ModelsYml(yaml::YamlDocument doc, runtime::fs::Snapshot snapshot) : doc_(std::move(doc)), snapshot_(std::move(snapshot)) {}

    yaml::YamlDocument doc_;
    runtime::fs::Snapshot snapshot_;
};

} // namespace loa::settings::models::models_yml
