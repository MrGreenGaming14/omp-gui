#pragma once

// Sign in: every provider of every harness with its authentication state, the sign in flow
// of subscription logins as a pure reducer over `LoginUpdate`s, and signing out by removing
// one stored sign in after the user picked it. API keys are never entered here; API key rows
// only say where the harness reads the key from.
// Port of `crates/loa-ui-settings/src/models/signin.rs` (state; the view is `SignInView`).

#include <cstdint>
#include <expected>
#include <optional>
#include <span>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/widgets.hpp"
#include "loa/view/settings_models.hpp"

namespace loa::ui::settings::models::signin {

using loa::view::settings::models::Credential;
using loa::view::settings::models::LoginState;
using loa::view::settings::models::LoginUpdate;
using loa::view::settings::models::ProviderEntry;
using loa::view::settings::models::ProviderKey;
using loa::view::settings::models::signin::Intent;

namespace local {
struct SignIn { ProviderKey key; friend bool operator==(const SignIn&, const SignIn&) = default; };
struct Cancel { ProviderKey key; friend bool operator==(const Cancel&, const Cancel&) = default; };
// Text typed into a running login's prompt.
struct Input { ProviderKey key; std::string value; friend bool operator==(const Input&, const Input&) = default; };
struct Submit { ProviderKey key; friend bool operator==(const Submit&, const Submit&) = default; };
// Returns a finished row (succeeded, failed, cancelled, signed out) to idle.
struct Dismiss { ProviderKey key; friend bool operator==(const Dismiss&, const Dismiss&) = default; };
// Lists a signed in row's stored sign ins to pick one to remove.
struct SignOut { ProviderKey key; friend bool operator==(const SignOut&, const SignOut&) = default; };
// Removes the picked stored sign in.
struct Remove { ProviderKey key; std::optional<std::int64_t> credential; friend bool operator==(const Remove&, const Remove&) = default; };
// Closes a row's sign out before anything was removed.
struct CancelSignOut { ProviderKey key; friend bool operator==(const CancelSignOut&, const CancelSignOut&) = default; };
} // namespace local

using Local = std::variant<local::SignIn, local::Cancel, local::Input, local::Submit, local::Dismiss, local::SignOut, local::Remove,
                           local::CancelSignOut>;
using Message = kit::widgets::Msg<Local, Intent>;

// Where a row's sign out is.
namespace sign_out {
// Reading the stored sign ins.
struct Reading { friend bool operator==(const Reading&, const Reading&) = default; };
// The stored sign ins, each with its own Remove.
struct Confirm { std::vector<Credential> credentials; friend bool operator==(const Confirm&, const Confirm&) = default; };
struct Removing { friend bool operator==(const Removing&, const Removing&) = default; };
// The outcome, shown until dismissed.
struct Done {
    std::string message;
    kit::Tone tone = kit::Tone::Muted;
    friend bool operator==(const Done&, const Done&) = default;
};
} // namespace sign_out

using SignOut = std::variant<sign_out::Reading, sign_out::Confirm, sign_out::Removing, sign_out::Done>;

// The provider rows and the login and sign out state of each; a missing key is idle.
class State : public kit::widgets::Screen<Local, Intent> {
  public:
    State() = default;
    explicit State(std::vector<ProviderEntry> providers) : providers_(std::move(providers)) {}

    // Replaces the rows; logins and sign outs of rows still present go on, except listed
    // stored sign ins, which the new rows may have changed.
    void set_providers(std::vector<ProviderEntry> providers);
    [[nodiscard]] std::span<const ProviderEntry> providers() const noexcept { return providers_; }
    [[nodiscard]] const LoginState& login_state(const ProviderKey& key) const;
    // The row's sign out, if any.
    [[nodiscard]] const SignOut* sign_out(const ProviderKey& key) const;
    // Whether any row has a sign out.
    [[nodiscard]] bool has_sign_outs() const noexcept { return !sign_outs_.empty(); }

    // The stored sign ins a row's sign out read. Ignored unless the row is reading them, so a
    // late answer of a cancelled sign out cannot bring it back.
    void credentials(const ProviderKey& key, std::expected<std::vector<Credential>, std::string> result);
    // The outcome of a row's removal: what still signs the provider in, or why it failed.
    void signed_out(const ProviderKey& key, std::expected<std::optional<std::string>, std::string> result);
    // Applies a running login's progress. Rows without a running login ignore it, so a late
    // update of a cancelled or finished login cannot bring it back.
    void login_update(const ProviderKey& key, LoginUpdate update);

    std::optional<Intent> reduce(Local m) override;
    // Only the URLs of a login at its browser step leave the screen.
    [[nodiscard]] bool guard(const Intent& e) const override;

  private:
    [[nodiscard]] bool sign_out_open(const ProviderKey& key) const;
    LoginState* login_mut(const ProviderKey& key);
    SignOut* sign_out_mut(const ProviderKey& key);
    void erase_login(const ProviderKey& key);
    void erase_sign_out(const ProviderKey& key);
    void put_login(const ProviderKey& key, LoginState s);
    void put_sign_out(const ProviderKey& key, SignOut s);

    std::vector<ProviderEntry> providers_;
    std::vector<std::pair<ProviderKey, LoginState>> logins_;
    std::vector<std::pair<ProviderKey, SignOut>> sign_outs_;
};

} // namespace loa::ui::settings::models::signin
