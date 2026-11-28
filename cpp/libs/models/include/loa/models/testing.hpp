#pragma once

// In-memory omp RPC and offline accounts for exercising model and login flows without a
// process (port of `loa-models` `testing.rs`). Only in `loa_models_testing`.

#include <chrono>
#include <memory>
#include <mutex>
#include <optional>
#include <set>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "loa/models/service.hpp"

namespace loa::models::testing {

namespace rpc = settings::models::rpc;

// The behavior of a fake login after its UI frames have been sent.
namespace script {
// Wait indefinitely until the caller cancels the login.
struct Hang {};
// Return the given result immediately.
struct Result {
    rpc::RpcResult<std::string> result;
};
// Succeed after any UI response.
struct AwaitResponse {};
// Succeed only when the response has the expected value.
struct AwaitInput {
    std::string expected;
};
// Complete after a delay, marking the provider authenticated.
struct CompleteAfter {
    std::chrono::steady_clock::duration delay;
};
} // namespace script
using LoginScript = std::variant<script::Hang, script::Result, script::AwaitResponse, script::AwaitInput, script::CompleteAfter>;

// Configurable fake implementing the same RPC port used by the application. Set the
// fields before sharing it.
class FakeOmp final : public rpc::OmpRpc {
  public:
    FakeOmp();
    ~FakeOmp() override;

    std::vector<rpc::LoginProvider> providers;
    std::vector<rpc::ModelInfo> models;
    std::vector<rpc::LoginUiRequest> login_frames;
    LoginScript script;
    // What `logout` reports still authenticating the provider.
    std::optional<std::string> remaining_source;

    // The stored credentials `get_logout_accounts` lists and `logout` removes.
    void set_credentials(std::vector<rpc::LogoutAccount> credentials);
    [[nodiscard]] std::vector<rpc::UiResponse> responses() const;
    [[nodiscard]] std::vector<std::pair<std::string, std::string>> selected() const;

    [[nodiscard]] contracts::Awaitable<rpc::RpcResult<std::vector<rpc::LoginProvider>>> get_login_providers() const override;
    [[nodiscard]] contracts::Awaitable<rpc::RpcResult<std::string>> login(std::string provider_id,
                                                                         std::shared_ptr<rpc::LoginUiChannel> ui) const override;
    [[nodiscard]] contracts::Awaitable<rpc::RpcResult<>> respond_ui(rpc::UiResponse response) const override;
    [[nodiscard]] contracts::Awaitable<rpc::RpcResult<std::vector<rpc::ModelInfo>>> get_available_models() const override;
    [[nodiscard]] contracts::Awaitable<rpc::RpcResult<rpc::ModelInfo>> set_model(std::string provider,
                                                                                std::string model_id) const override;
    [[nodiscard]] contracts::Awaitable<rpc::RpcResult<std::vector<rpc::LogoutAccount>>> get_logout_accounts(
        std::string provider_id) const override;
    [[nodiscard]] contracts::Awaitable<rpc::RpcResult<std::optional<std::string>>> logout(std::string provider_id,
                                                                                        std::int64_t credential_id) const override;

  private:
    struct State;
    std::unique_ptr<State> state_;
};

// An omp account that runs nothing: provider logins are refused and the default model
// reads as unset.
class OfflineOmpAccount final : public OmpProviderAccount {
  public:
    [[nodiscard]] std::string_view id() const override { return "omp"; }
    [[nodiscard]] std::string_view name() const override { return "omp"; }
    [[nodiscard]] contracts::Awaitable<contracts::SignInState> sign_in_state() const override;
    [[nodiscard]] contracts::Result<std::unique_ptr<LoginStrategy>> login(contracts::LoginKind kind) const override;
    [[nodiscard]] std::optional<contracts::CommandSpec> logout_command() const override { return std::nullopt; }
    [[nodiscard]] std::span<const std::string_view> efforts() const override;
    [[nodiscard]] std::optional<std::filesystem::path> config_path() const override { return std::nullopt; }
    [[nodiscard]] contracts::Result<contracts::ModelSettings> read_model() const override { return contracts::ModelSettings{}; }
    [[nodiscard]] contracts::Result<> write_model(const contracts::ModelSettings& m) const override;
    [[nodiscard]] contracts::Result<std::unique_ptr<LoginStrategy>> login_provider(rpc::LoginProvider provider) const override;
};

// Accepts every file unless it contains `reject-me` (then omp rejects `config.yml` with
// "bad role").
class RejectingValidator final : public SettingsValidator {
  public:
    [[nodiscard]] contracts::Awaitable<settings::models::Result<>> check_config(
        std::string text, std::vector<std::pair<std::string, runtime::json::Value>> expected) const override;
    [[nodiscard]] contracts::Awaitable<settings::models::Result<>> check_models(std::string text) const override;
};

// Opens nothing; every URL fails with "no browser in tests".
class NoBrowser final : public UrlOpener {
  public:
    [[nodiscard]] std::expected<void, std::string> open_url(std::string_view url) const override;
};

// An environment where no variable is set.
class NoEnv final : public EnvironmentPresence {
  public:
    [[nodiscard]] bool contains(std::string_view name) const override;
};

} // namespace loa::models::testing
