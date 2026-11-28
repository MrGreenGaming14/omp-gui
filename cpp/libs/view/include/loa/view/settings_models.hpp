#pragma once

// The models screen: provider sign in (`signin`), the session's model (`picker`)
// and omp's model settings (`settings`), mirrored from the model settings domain.

#include <array>
#include <compare>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/core/schema.hpp"

namespace loa::view::settings::models {

// One model, named the way omp's `set_model` takes it: a provider id and the model id within
// that provider.
struct ModelRef {
    std::string provider;
    std::string id;

    // The `provider/id` selector.
    [[nodiscard]] std::string selector() const;

    friend bool operator==(const ModelRef&, const ModelRef&) = default;
    friend auto operator<=>(const ModelRef&, const ModelRef&) = default;
};

// A model the user can pick.
struct ModelInfo {
    ModelRef model;
    // Display name, e.g. "Claude Sonnet 4.5". Empty falls back to the id.
    std::string name;
    // Display name of the provider, e.g. "Anthropic". Empty falls back to the provider id.
    std::string provider_name;

    [[nodiscard]] const std::string& display_name() const noexcept;
    [[nodiscard]] const std::string& provider_display_name() const noexcept;

    friend bool operator==(const ModelInfo&, const ModelInfo&) = default;
};

// A model role (`default`, `smol`, `plan`, …) and the model it resolves to.
struct RoleAssignment {
    std::string role;
    ModelRef model;

    friend bool operator==(const RoleAssignment&, const RoleAssignment&) = default;
};

// The models and role assignments the picker offers.
struct Catalog {
    std::vector<ModelInfo> models;
    std::vector<RoleAssignment> roles;

    friend bool operator==(const Catalog&, const Catalog&) = default;
};

// The session's model as its `<session>` element records it (`model`, `model_role`).
struct SessionModel {
    // Model selector, `provider/id` or a bare id some harnesses write.
    std::optional<std::string> model;
    // The role the model was picked through, when it follows one.
    std::optional<std::string> role;

    friend bool operator==(const SessionModel&, const SessionModel&) = default;
};

// A model picked for the session: through `role` it follows that role, without one it follows none.
struct SessionModelChange {
    ModelRef model;
    std::optional<std::string> role;

    friend bool operator==(const SessionModelChange&, const SessionModelChange&) = default;
};

// Identifies one sign in row: the harness that owns the login and its provider id.
struct ProviderKey {
    std::string harness;
    std::string provider;

    friend bool operator==(const ProviderKey&, const ProviderKey&) = default;
};

// How a provider authenticates.
namespace auth {
// Subscription login (OAuth) run by the harness.
struct Login {
    bool signed_in = false;
    std::optional<std::string> account;
    friend bool operator==(const Login&, const Login&) = default;
};
// Provider authenticated by an API key the harness reads itself.
struct ApiKey {
    // Where the key was found; `std::nullopt` when no key is configured.
    std::optional<std::string> configured_via;
    // Tells the user where to put the key.
    std::string instructions;
    friend bool operator==(const ApiKey&, const ApiKey&) = default;
};
// No sign in can be offered now.
struct Unavailable {
    std::string reason;
    friend bool operator==(const Unavailable&, const Unavailable&) = default;
};
} // namespace auth

using Auth = std::variant<auth::Login, auth::ApiKey, auth::Unavailable>;

// One provider row.
struct ProviderEntry {
    ProviderKey key;
    // Display name of the provider, e.g. "Anthropic".
    std::string name;
    // Display name of the owning harness; rows are grouped under it.
    std::string harness_name;
    Auth auth;

    friend bool operator==(const ProviderEntry&, const ProviderEntry&) = default;
};

// A stored sign in that signing out of a row removes; only its description.
struct Credential {
    // Names the credential to its harness; `std::nullopt` for a harness that keeps one sign in.
    std::optional<std::int64_t> id;
    std::string label;
    std::string detail;
    // Whether the harness signs in with it now.
    bool active = false;

    friend bool operator==(const Credential&, const Credential&) = default;
};

// A pending prompt of a running login and what the user typed so far.
struct LoginPrompt {
    std::string message;
    std::optional<std::string> placeholder;
    std::string input;

    friend bool operator==(const LoginPrompt&, const LoginPrompt&) = default;
};

// What a row shows about its login.
namespace login_state {
struct Idle {
    friend bool operator==(const Idle&, const Idle&) = default;
};
// Sign in requested, waiting for the URL; progress so far is carried into `Browser`.
struct Starting {
    std::optional<std::string> progress;
    friend bool operator==(const Starting&, const Starting&) = default;
};
// Browser step: URL shown and copyable, waiting for the user to finish.
struct Browser {
    std::string url;
    // The URL the Copy button writes: the launch URL when the harness sent one, the full URL otherwise.
    std::string copy_url;
    std::optional<std::string> instructions;
    // The latest progress message.
    std::optional<std::string> progress;
    std::optional<LoginPrompt> prompt;
    friend bool operator==(const Browser&, const Browser&) = default;
};
struct Succeeded {
    std::optional<std::string> account;
    friend bool operator==(const Succeeded&, const Succeeded&) = default;
};
struct Failed {
    std::string message;
    friend bool operator==(const Failed&, const Failed&) = default;
};
struct Cancelled {
    friend bool operator==(const Cancelled&, const Cancelled&) = default;
};
} // namespace login_state

struct LoginState {
    std::variant<login_state::Idle, login_state::Starting, login_state::Browser, login_state::Succeeded,
                 login_state::Failed, login_state::Cancelled>
        value;

    // Whether a login is running for the row.
    [[nodiscard]] bool is_running() const noexcept;

    friend bool operator==(const LoginState&, const LoginState&) = default;
};

// Progress of a running login, as the harness reports it.
namespace login_update {
struct Url {
    std::string url;
    std::optional<std::string> launch_url;
    std::optional<std::string> instructions;
    friend bool operator==(const Url&, const Url&) = default;
};
struct Progress {
    std::string message;
    friend bool operator==(const Progress&, const Progress&) = default;
};
struct Prompt {
    std::string message;
    std::optional<std::string> placeholder;
    friend bool operator==(const Prompt&, const Prompt&) = default;
};
struct Succeeded {
    std::optional<std::string> account;
    friend bool operator==(const Succeeded&, const Succeeded&) = default;
};
struct Failed {
    std::string message;
    friend bool operator==(const Failed&, const Failed&) = default;
};
struct Cancelled {
    friend bool operator==(const Cancelled&, const Cancelled&) = default;
};
} // namespace login_update

using LoginUpdate = std::variant<login_update::Url, login_update::Progress, login_update::Prompt,
                                 login_update::Succeeded, login_update::Failed, login_update::Cancelled>;

// The default model and effort another harness keeps in its own config file. Empty values
// leave the choice to the harness.
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

// A model role and its ordered chain: `chain[0]` is the primary model, the rest its fallbacks.
struct RoleChain {
    std::string role;
    std::vector<std::string> chain;

    friend bool operator==(const RoleChain&, const RoleChain&) = default;
};

// Whether a model selector is enabled.
struct ModelToggle {
    std::string selector;
    bool enabled = false;

    friend bool operator==(const ModelToggle&, const ModelToggle&) = default;
};

// A provider and whether it is enabled, in provider order.
struct ProviderToggle {
    std::string id;
    std::string name;
    bool enabled = false;

    friend bool operator==(const ProviderToggle&, const ProviderToggle&) = default;
};

// A typed override value: text, int, float or bool.
using FieldValue = std::variant<std::string, std::int64_t, double, bool>;

// One overridden model field, e.g. `contextWindow: 200000`.
struct OverrideField {
    std::string key;
    FieldValue value;

    friend bool operator==(const OverrideField&, const OverrideField&) = default;
};

// The override fields set for one model selector.
struct ModelOverride {
    std::string selector;
    std::vector<OverrideField> fields;

    friend bool operator==(const ModelOverride&, const ModelOverride&) = default;
};

// How a custom provider authenticates.
enum class AuthMode : std::uint8_t { ApiKey, None, OAuth };

// A custom provider. The API key is only ever the name of the environment variable holding it.
struct CustomProvider {
    std::string id;
    std::string base_url;
    std::string api;
    AuthMode auth = AuthMode::ApiKey;
    std::string api_key_env;
    std::vector<std::string> models;

    friend bool operator==(const CustomProvider&, const CustomProvider&) = default;
};

// Everything the model settings page edits.
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

// A problem the app (or omp) reported for a saved settings value.
struct ValidationError {
    std::string location;
    std::string message;

    friend bool operator==(const ValidationError&, const ValidationError&) = default;
};

// Names an override field.
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

// The override fields the settings page edits, with the type each is edited as.
inline constexpr std::array<std::pair<std::string_view, OverrideKey>, 8> OVERRIDE_FIELDS{{
    {"name", OverrideKey::Name},
    {"contextWindow", OverrideKey::ContextWindow},
    {"maxContextWindow", OverrideKey::MaxContextWindow},
    {"maxTokens", OverrideKey::MaxTokens},
    {"reasoning", OverrideKey::Reasoning},
    {"contextPromotionTarget", OverrideKey::ContextPromotionTarget},
    {"compactionModel", OverrideKey::CompactionModel},
    {"preferWebsockets", OverrideKey::PreferWebsockets},
}};

// Whether `name` looks like an environment variable name by convention.
[[nodiscard]] bool is_env_var_name(std::string_view name) noexcept;

// Whether `id` is a provider id omp accepts: letters, digits, `-`, `_` or `.`.
[[nodiscard]] bool is_provider_id(std::string_view id) noexcept;

} // namespace loa::view::settings::models

// The sign in page.
namespace loa::view::settings::models::signin {

// What the app acts on.
namespace intent {
// Start this provider's login with its harness.
struct SignIn {
    ProviderKey key;
    friend bool operator==(const SignIn&, const SignIn&) = default;
};
// Abort the running login.
struct Cancel {
    ProviderKey key;
    friend bool operator==(const Cancel&, const Cancel&) = default;
};
// Answer the running login's prompt.
struct SubmitInput {
    ProviderKey key;
    std::string text;
    friend bool operator==(const SubmitInput&, const SubmitInput&) = default;
};
// Open a login's URL in the system browser.
struct OpenUrl {
    std::string url;
    friend bool operator==(const OpenUrl&, const OpenUrl&) = default;
};
// Put a login's URL on the clipboard.
struct CopyUrl {
    std::string url;
    friend bool operator==(const CopyUrl&, const CopyUrl&) = default;
};
// List the stored sign ins of a signed in provider, to pick the one to remove.
struct SignOut {
    ProviderKey key;
    friend bool operator==(const SignOut&, const SignOut&) = default;
};
// Remove the stored sign in `credential` (a `Credential::id`) of the provider.
struct RemoveCredential {
    ProviderKey key;
    std::optional<std::int64_t> credential;
    friend bool operator==(const RemoveCredential&, const RemoveCredential&) = default;
};
} // namespace intent

using Intent = std::variant<intent::SignIn, intent::Cancel, intent::SubmitInput, intent::OpenUrl, intent::CopyUrl,
                            intent::SignOut, intent::RemoveCredential>;

} // namespace loa::view::settings::models::signin

// The model picker.
namespace loa::view::settings::models::picker {

// What a pick changes.
namespace target {
// The session's model; roles are rows too, and picking one makes the session follow it.
struct Session {
    friend bool operator==(const Session&, const Session&) = default;
};
// The model a role resolves to.
struct Role {
    std::string role;
    friend bool operator==(const Role&, const Role&) = default;
};
} // namespace target

using Target = std::variant<target::Session, target::Role>;

// What the app acts on.
namespace intent {
// Switch the session's model, following the change's role (or none).
struct SetSessionModel {
    SessionModelChange change;
    friend bool operator==(const SetSessionModel&, const SetSessionModel&) = default;
};
// Assign `model` to `role` in the harness config.
struct SetRoleModel {
    std::string role;
    ModelRef model;
    friend bool operator==(const SetRoleModel&, const SetRoleModel&) = default;
};
// Dismissed without a pick.
struct Close {
    friend bool operator==(const Close&, const Close&) = default;
};
} // namespace intent

using Intent = std::variant<intent::SetSessionModel, intent::SetRoleModel, intent::Close>;

} // namespace loa::view::settings::models::picker

// The model settings page.
namespace loa::view::settings::models::settings {

// What the app acts on.
namespace intent {
// Write these settings, then report the result back to the page.
struct Save {
    ModelSettings settings;
    friend bool operator==(const Save&, const Save&) = default;
};
// The unsaved changes were dropped.
struct Reverted {
    friend bool operator==(const Reverted&, const Reverted&) = default;
};
} // namespace intent

using Intent = std::variant<intent::Save, intent::Reverted>;

} // namespace loa::view::settings::models::settings

namespace loa::core {
LOA_CORE_STR_ENUM(loa::view::settings::models::AuthMode, "apiKey", "none", "oauth");
LOA_CORE_STR_ENUM(loa::view::settings::models::OverrideKey, "name", "contextWindow", "maxContextWindow", "maxTokens",
                  "reasoning", "input", "contextPromotionTarget", "compactionModel", "preferWebsockets", "cost");
} // namespace loa::core
