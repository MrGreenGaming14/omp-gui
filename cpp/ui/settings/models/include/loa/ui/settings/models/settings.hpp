#pragma once

// Model settings: model roles with their fallback chains, enabled models, provider order, per
// model overrides (a kit form over omp's `OVERRIDE_FIELDS`), custom providers and the other
// harnesses' own default models. The app loads `ModelSettings`, writes what `intent::Save`
// carries and answers with `State::saved` or `State::set_errors`.
// Port of `crates/loa-ui-settings/src/models/settings.rs` (state; the view is `SettingsView`).

#include <array>
#include <cstddef>
#include <expected>
#include <map>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/ui/kit/widgets.hpp"
#include "loa/view/settings_models.hpp"

namespace loa::ui::settings::models::settings {

using loa::view::settings::models::AuthMode;
using loa::view::settings::models::CustomProvider;
using loa::view::settings::models::FieldValue;
using loa::view::settings::models::ModelSettings;
using loa::view::settings::models::OVERRIDE_FIELDS;
using loa::view::settings::models::OverrideField;
using loa::view::settings::models::OverrideKey;
using loa::view::settings::models::ValidationError;
using loa::view::settings::models::settings::Intent;

// The effort list's entry for "no effort set".
inline constexpr std::string_view HARNESS_DEFAULT = "default";

// Field problems by field key, sorted by key.
using Errors = std::map<std::string, std::string, std::less<>>;

// One kit field per editable override, in `OVERRIDE_FIELDS` order.
[[nodiscard]] std::span<const kit::widgets::Field> override_form();
// The custom provider form: id, base_url, api, api_key_env, models.
[[nodiscard]] std::span<const kit::widgets::Field> provider_form();

// `raw` as the value of override `key`; empty leaves it unset.
[[nodiscard]] std::expected<std::optional<FieldValue>, std::string> parse(OverrideKey key, std::string_view raw);

// An override's values as typed, parallel to `OVERRIDE_FIELDS`.
struct OverrideDraft : kit::widgets::Form {
    std::string selector;
    std::vector<std::string> values;

    OverrideDraft() = default;
    OverrideDraft(std::string s, std::vector<std::string> v) : selector(std::move(s)), values(std::move(v)) {}
    explicit OverrideDraft(const loa::view::settings::models::ModelOverride& o);
    // The typed values as override fields, and the ones that don't parse.
    [[nodiscard]] std::pair<std::vector<OverrideField>, Errors> parsed() const;

    [[nodiscard]] std::span<const kit::widgets::Field> fields() const override { return override_form(); }
    [[nodiscard]] std::string get(std::string_view key) const override;
    std::expected<void, std::string> set(std::string_view key, std::string_view value) override;

    friend bool operator==(const OverrideDraft& a, const OverrideDraft& b) { return a.selector == b.selector && a.values == b.values; }
};

// A custom provider as typed, parallel to `provider_form()`; models are comma separated.
struct ProviderDraft : kit::widgets::Form {
    std::array<std::string, 5> values;
    AuthMode auth = AuthMode::ApiKey;

    ProviderDraft() = default;
    explicit ProviderDraft(const CustomProvider& p);
    [[nodiscard]] CustomProvider provider() const;

    [[nodiscard]] std::span<const kit::widgets::Field> fields() const override { return provider_form(); }
    [[nodiscard]] std::string get(std::string_view key) const override;
    std::expected<void, std::string> set(std::string_view key, std::string_view value) override;

    friend bool operator==(const ProviderDraft& a, const ProviderDraft& b) { return a.values == b.values && a.auth == b.auth; }
};

// Form problems of a custom provider among `others` that omp's own validation would only
// report less clearly. An API key pasted as its variable is refused, so it never reaches a file.
[[nodiscard]] Errors provider_errors(const CustomProvider& p, std::span<const CustomProvider> others);

// The editable form of `ModelSettings`: `base` holds roles, toggles and harnesses; overrides
// and custom providers stay as typed text.
struct Draft {
    ModelSettings base;
    std::vector<OverrideDraft> overrides;
    std::vector<ProviderDraft> custom;

    Draft() = default;
    explicit Draft(const ModelSettings& s);
    [[nodiscard]] std::vector<CustomProvider> providers() const;
    // Each provider's form errors.
    [[nodiscard]] std::vector<Errors> provider_errors() const;
    // The settings, or every problem keeping them from being saved.
    [[nodiscard]] std::expected<ModelSettings, std::vector<std::string>> settings() const;

    friend bool operator==(const Draft&, const Draft&) = default;
};

namespace local {
struct NewRole { std::string name; friend bool operator==(const NewRole&, const NewRole&) = default; };
struct AddRole { friend bool operator==(const AddRole&, const AddRole&) = default; };
struct RemoveRole { std::size_t role; friend bool operator==(const RemoveRole&, const RemoveRole&) = default; };
struct RenameRole { std::size_t role; std::string name; friend bool operator==(const RenameRole&, const RenameRole&) = default; };
// Text typed into a role's chain input.
struct ChainInput { std::size_t role; std::string value; friend bool operator==(const ChainInput&, const ChainInput&) = default; };
// Appends a picked selector to a role's chain, or the typed one with `nullopt`.
struct AddChain { std::size_t role; std::optional<std::string> picked; friend bool operator==(const AddChain&, const AddChain&) = default; };
struct RemoveChain { std::size_t role, entry; friend bool operator==(const RemoveChain&, const RemoveChain&) = default; };
// Moves a role's chain entry.
struct MoveChain { std::size_t role, from, to; friend bool operator==(const MoveChain&, const MoveChain&) = default; };
struct ModelToggled { std::size_t model; bool on; friend bool operator==(const ModelToggled&, const ModelToggled&) = default; };
struct ProviderToggled { std::size_t provider; bool on; friend bool operator==(const ProviderToggled&, const ProviderToggled&) = default; };
struct MoveProvider { std::size_t from, to; friend bool operator==(const MoveProvider&, const MoveProvider&) = default; };
struct NewOverride { std::string selector; friend bool operator==(const NewOverride&, const NewOverride&) = default; };
// Overrides a picked model, or the typed one with `nullopt`.
struct AddOverride { std::optional<std::string> picked; friend bool operator==(const AddOverride&, const AddOverride&) = default; };
struct RemoveOverride { std::size_t index; friend bool operator==(const RemoveOverride&, const RemoveOverride&) = default; };
struct OverrideEdit { std::size_t index; std::string key, value; friend bool operator==(const OverrideEdit&, const OverrideEdit&) = default; };
struct AddProvider { friend bool operator==(const AddProvider&, const AddProvider&) = default; };
struct RemoveProvider { std::size_t index; friend bool operator==(const RemoveProvider&, const RemoveProvider&) = default; };
struct ProviderEdit { std::size_t index; std::string key, value; friend bool operator==(const ProviderEdit&, const ProviderEdit&) = default; };
struct ProviderAuth { std::size_t index; AuthMode auth; friend bool operator==(const ProviderAuth&, const ProviderAuth&) = default; };
struct HarnessModel { std::size_t index; std::string model; friend bool operator==(const HarnessModel&, const HarnessModel&) = default; };
// `HARNESS_DEFAULT` clears the effort.
struct HarnessEffort { std::size_t index; std::string effort; friend bool operator==(const HarnessEffort&, const HarnessEffort&) = default; };
struct Save { friend bool operator==(const Save&, const Save&) = default; };
struct Revert { friend bool operator==(const Revert&, const Revert&) = default; };
} // namespace local

using Local = std::variant<local::NewRole, local::AddRole, local::RemoveRole, local::RenameRole, local::ChainInput, local::AddChain,
                           local::RemoveChain, local::MoveChain, local::ModelToggled, local::ProviderToggled, local::MoveProvider,
                           local::NewOverride, local::AddOverride, local::RemoveOverride, local::OverrideEdit, local::AddProvider,
                           local::RemoveProvider, local::ProviderEdit, local::ProviderAuth, local::HarnessModel, local::HarnessEffort,
                           local::Save, local::Revert>;
using Message = kit::widgets::Msg<Local, Intent>;

class State : public kit::widgets::Screen<Local, Intent> {
  public:
    State(ModelSettings settings, std::vector<std::string> known);

    // The app wrote `settings`: they become the baseline and the draft.
    void saved(const ModelSettings& settings);
    // Shows what the app (or omp) reported for the last write.
    void set_errors(std::vector<ValidationError> errors) { app_errors_ = std::move(errors); }
    [[nodiscard]] std::span<const ValidationError> errors() const noexcept { return app_errors_; }
    [[nodiscard]] std::span<const std::string> local_errors() const noexcept { return local_errors_; }
    [[nodiscard]] bool is_dirty() const { return !(draft_ == baseline_); }
    [[nodiscard]] std::expected<ModelSettings, std::vector<std::string>> draft_settings() const { return draft_.settings(); }

    [[nodiscard]] const Draft& draft() const noexcept { return draft_; }
    [[nodiscard]] std::span<const std::string> known() const noexcept { return known_; }
    [[nodiscard]] const std::string& new_role() const noexcept { return new_role_; }
    [[nodiscard]] std::span<const std::string> chain_inputs() const noexcept { return chain_inputs_; }
    [[nodiscard]] const std::string& new_override() const noexcept { return new_override_; }

    std::optional<Intent> reduce(Local m) override;
    // Every event comes from the reducer.
    [[nodiscard]] bool guard(const Intent&) const override { return false; }

  private:
    State(Draft draft, std::vector<std::string> known);
    // Back to the baseline, every input and error cleared.
    void reset();

    Draft baseline_;
    Draft draft_;
    // Selectors offered when adding chain entries and overrides.
    std::vector<std::string> known_;
    std::string new_role_;
    // Each role's chain input, parallel to its roles.
    std::vector<std::string> chain_inputs_;
    std::string new_override_;
    // Problems the last Save found in the draft.
    std::vector<std::string> local_errors_;
    // Problems the app reported for the last write.
    std::vector<ValidationError> app_errors_;
};

} // namespace loa::ui::settings::models::settings
