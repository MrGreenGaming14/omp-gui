#pragma once

// Shared process, configuration and login support for harness accounts (port of
// `account_support/mod.rs`).

#include <string_view>

#include "loa/contracts/account.hpp"
#include "loa/settings/models/error.hpp"

namespace loa::harness::account_support {

using loa::contracts::LoginKind;
using loa::contracts::SignInMethod;
using loa::contracts::SignInState;

// Checks a model name before it is written to a harness config: not empty and free of
// whitespace and control characters.
[[nodiscard]] loa::settings::models::Result<> validate_model_name(std::string_view field, std::string_view name);

} // namespace loa::harness::account_support
