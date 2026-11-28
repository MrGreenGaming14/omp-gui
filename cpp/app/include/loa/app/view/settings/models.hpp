#pragma once

// Model settings, sign in and picker data as `loa::view::settings::models` views, and the edited
// views back as domain values.

#include "loa/contracts/account.hpp"
#include "loa/settings/models/login.hpp"
#include "loa/settings/models/selector.hpp"
#include "loa/settings/models/settings.hpp"
#include "loa/view/settings_models.hpp"

namespace loa::app::view::settings {

[[nodiscard]] ::loa::view::settings::models::ModelRef model_ref_to_view(const ::loa::settings::models::selector::ModelRef& model);
[[nodiscard]] ::loa::settings::models::selector::ModelRef model_ref_from_view(::loa::view::settings::models::ModelRef model);
[[nodiscard]] ::loa::view::settings::models::Catalog catalog_to_view(const ::loa::settings::models::selector::Catalog& catalog);
[[nodiscard]] ::loa::view::settings::models::ProviderKey provider_key_to_view(const ::loa::settings::models::login::ProviderKey& key);
[[nodiscard]] ::loa::settings::models::login::ProviderKey provider_key_from_view(::loa::view::settings::models::ProviderKey key);
[[nodiscard]] ::loa::view::settings::models::ProviderEntry provider_to_view(const ::loa::settings::models::login::ProviderEntry& entry);
[[nodiscard]] ::loa::view::settings::models::Credential credential_to_view(const ::loa::settings::models::login::Credential& credential);
[[nodiscard]] ::loa::view::settings::models::LoginUpdate login_update_to_view(::loa::contracts::LoginUpdate update);
[[nodiscard]] ::loa::view::settings::models::ValidationError validation_error_to_view(const ::loa::settings::models::settings::ValidationError& error);
[[nodiscard]] ::loa::view::settings::models::ModelSettings settings_to_view(const ::loa::settings::models::settings::ModelSettings& s);
[[nodiscard]] ::loa::settings::models::settings::ModelSettings settings_from_view(::loa::view::settings::models::ModelSettings s);

} // namespace loa::app::view::settings
