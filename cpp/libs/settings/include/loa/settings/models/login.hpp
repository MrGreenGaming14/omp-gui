#pragma once

// Login state and strategies shared by the model service and sign-in screen
// (port of `models/login.rs`).

#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <variant>

#include "loa/contracts/account.hpp"
#include "loa/settings/models/rpc.hpp"

namespace loa::settings::models::login {

using contracts::LoginControl;
using contracts::LoginStrategy;
using contracts::LoginUpdate;
using contracts::UrlOpener;

// Identifies one row: the harness that owns the login and its provider id.
struct ProviderKey {
    std::string harness;
    std::string provider;
    friend bool operator==(const ProviderKey&, const ProviderKey&) = default;
    friend auto operator<=>(const ProviderKey&, const ProviderKey&) = default;
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
    // Where the key was found (env var name or config key); nullopt when none is configured.
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

// One provider row as the app reports it.
struct ProviderEntry {
    ProviderKey key;
    // Display name of the provider, e.g. "Anthropic".
    std::string name;
    // Display name of the owning harness; rows are grouped under it.
    std::string harness_name;
    Auth auth;
    friend bool operator==(const ProviderEntry&, const ProviderEntry&) = default;
};

// A stored sign in that signing out of a row removes (description only).
struct Credential {
    // omp's credential id; nullopt for a harness that keeps one sign in.
    std::optional<std::int64_t> id;
    std::string label;
    std::string detail;
    // Whether the harness signs in with it now.
    bool active = false;
    friend bool operator==(const Credential&, const Credential&) = default;
};

// omp's sign-in account: a HarnessAccount that also logs in to one of the
// OAuth providers omp lists.
class OmpProviderAccount : public contracts::HarnessAccount {
  public:
    [[nodiscard]] virtual contracts::Result<std::unique_ptr<LoginStrategy>> login_provider(rpc::LoginProvider provider) const = 0;
};

} // namespace loa::settings::models::login
