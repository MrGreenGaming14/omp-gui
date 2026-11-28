#pragma once

// Sign-in, model config and construction of one harness (ports only; `loa::harness` implements them).

#include <expected>
#include <filesystem>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/harness.hpp"

namespace loa::contracts {

// The kind of credentials a harness is signed in with.
struct SignInMethod {
    enum class Kind { Subscription, ApiKey, ApiKeyHelper, OAuthToken, ThirdParty, Other };
    Kind kind = Kind::Other;
    // `Other`: a safe harness-supplied method identifier, or `unknown`.
    std::string other;
    friend bool operator==(const SignInMethod&, const SignInMethod&) = default;
};

// Whether a harness has usable credentials.
struct SignedIn {
    // The kind of credentials in use.
    SignInMethod method;
    friend bool operator==(const SignedIn&, const SignedIn&) = default;
};
struct SignedOut {
    friend bool operator==(const SignedOut&, const SignedOut&) = default;
};
// Never holds CLI output that could contain a credential.
struct SignInUnknown {
    std::string reason;
    friend bool operator==(const SignInUnknown&, const SignInUnknown&) = default;
};
using SignInState = std::variant<SignedIn, SignedOut, SignInUnknown>;

// A way to sign in.
enum class LoginKind { Browser, DeviceCode, Console, Sso };

// A program invocation; the child inherits the app's environment plus `env`.
struct CommandSpec {
    std::filesystem::path program;
    std::vector<std::string> args;
    std::vector<std::pair<std::string, std::string>> env;
    std::optional<std::filesystem::path> current_dir;

    // A command with no extra environment or working directory.
    [[nodiscard]] static CommandSpec make(std::filesystem::path program, std::vector<std::string> args) {
        return {std::move(program), std::move(args), {}, std::nullopt};
    }
    // The program as text, for error messages.
    [[nodiscard]] std::string program_name() const { return program.string(); }

    friend bool operator==(const CommandSpec&, const CommandSpec&) = default;
};

// The model and effort a harness is configured with outside any session.
struct ModelSettings {
    std::optional<std::string> model;
    std::optional<std::string> effort;
    friend bool operator==(const ModelSettings&, const ModelSettings&) = default;
};

// What the sign-in screen sends to a running login.
struct SubmitInput {
    std::string text;
    friend bool operator==(const SubmitInput&, const SubmitInput&) = default;
};
struct CancelLogin {
    friend bool operator==(const CancelLogin&, const CancelLogin&) = default;
};
using LoginControl = std::variant<SubmitInput, CancelLogin>;

// Progress a running login reports.
namespace login {
struct Url {
    std::string url;
    std::optional<std::string> launch_url;
    std::optional<std::string> instructions;
    friend bool operator==(const Url&, const Url&) = default;
};
struct Progress {
    std::string text;
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
    std::string reason;
    friend bool operator==(const Failed&, const Failed&) = default;
};
struct Cancelled {
    friend bool operator==(const Cancelled&, const Cancelled&) = default;
};
} // namespace login
using LoginUpdate = std::variant<login::Url, login::Progress, login::Prompt, login::Succeeded, login::Failed, login::Cancelled>;

// The two ends a login runs between; defined in `loa/contracts/login_channels.hpp`
// (Asio channels: control in, updates out).
struct LoginChannels;

// One login flow, run to its end. The strategy must outlive the coroutine.
class LoginStrategy {
  public:
    virtual ~LoginStrategy() = default;
    [[nodiscard]] virtual Awaitable<void> run(std::shared_ptr<LoginChannels> channels) = 0;
};

// Opens a web URL without waiting for the browser.
class UrlOpener {
  public:
    virtual ~UrlOpener() = default;
    [[nodiscard]] virtual std::expected<void, std::string> open_url(std::string_view url) const = 0;
};

// Sign-in state and model config of one harness.
class HarnessAccount {
  public:
    virtual ~HarnessAccount() = default;
    // Stable sign-in and settings row identifier.
    [[nodiscard]] virtual std::string_view id() const = 0;
    [[nodiscard]] virtual std::string_view name() const = 0;
    [[nodiscard]] virtual Awaitable<SignInState> sign_in_state() const = 0;
    [[nodiscard]] virtual Result<std::unique_ptr<LoginStrategy>> login(LoginKind kind) const = 0;
    [[nodiscard]] virtual std::optional<CommandSpec> logout_command() const = 0;
    [[nodiscard]] virtual std::span<const std::string_view> efforts() const = 0;
    // Config file displayed by the settings screen, if this account has one.
    [[nodiscard]] virtual std::optional<std::filesystem::path> config_path() const = 0;
    [[nodiscard]] virtual Result<ModelSettings> read_model() const = 0;
    [[nodiscard]] virtual Result<> write_model(const ModelSettings& m) const = 0;
};

// Per-harness behavior the app queries instead of comparing harness ids.
struct HarnessTraits {
    std::span<const std::string_view> approval_modes;
    bool has_roles = false;
    bool server_url = false;
};

// Builds the adapter and account of one harness; the registry entry for it.
class HarnessFactory {
  public:
    virtual ~HarnessFactory() = default;
    [[nodiscard]] virtual std::string_view id() const = 0;
    [[nodiscard]] virtual std::string_view label() const = 0;
    // Icon token, e.g. `harness.claude`.
    [[nodiscard]] virtual std::string_view icon_token() const = 0;
    [[nodiscard]] virtual std::string_view default_program() const = 0;
    [[nodiscard]] virtual std::shared_ptr<Adapter> build(std::optional<std::filesystem::path> exe) const = 0;
    [[nodiscard]] virtual std::shared_ptr<HarnessAccount> account(std::optional<std::filesystem::path> exe) const = 0;
    [[nodiscard]] virtual HarnessTraits traits() const = 0;
};

} // namespace loa::contracts
