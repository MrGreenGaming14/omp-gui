#pragma once

#include <chrono>
#include <filesystem>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <utility>
#include <vector>

#include "loa/harness/omp/rpc/client.hpp"
#include "loa/settings/models/files.hpp"
#include "loa/settings/models/login.hpp"
#include "loa/settings/models/rpc_channel.hpp"

namespace loa::harness::omp {

struct OmpLaunch : settings::models::files::OmpFilesResolver {
    std::filesystem::path executable = "omp";
    std::optional<std::filesystem::path> cwd;
    std::vector<std::pair<std::string, std::string>> env;
    [[nodiscard]] contracts::Awaitable<settings::models::Result<settings::models::files::OmpFiles>> resolve() const override;
};

// An account-only RPC process, separate from chat sessions.
class OmpClient final : public settings::models::rpc::OmpRpc {
  public:
    [[nodiscard]] static contracts::Awaitable<settings::models::rpc::RpcResult<std::shared_ptr<OmpClient>>> spawn(const OmpLaunch& launch);
    ~OmpClient() override;
    [[nodiscard]] bool has_exited() const;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<std::optional<settings::models::rpc::ModelInfo>>> current_model() const;
    [[nodiscard]] contracts::Awaitable<void> shutdown(std::chrono::milliseconds grace) const;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<std::vector<settings::models::rpc::LoginProvider>>> get_login_providers() const override;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<std::string>> login(std::string provider_id, std::shared_ptr<settings::models::rpc::LoginUiChannel> ui) const override;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<>> respond_ui(settings::models::rpc::UiResponse response) const override;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<std::vector<settings::models::rpc::ModelInfo>>> get_available_models() const override;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<settings::models::rpc::ModelInfo>> set_model(std::string provider, std::string model_id) const override;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<std::vector<settings::models::rpc::LogoutAccount>>> get_logout_accounts(std::string provider_id) const override;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<std::optional<std::string>>> logout(std::string provider_id, std::int64_t credential_id) const override;
  private:
    explicit OmpClient(rpc::RpcClient client) : client_(std::move(client)) {}
    static contracts::Awaitable<void> forward(rpc::Events events, rpc::UiResponder responder, std::shared_ptr<struct LoginSlot> slot);
    rpc::RpcClient client_;
    std::shared_ptr<struct LoginSlot> login_;
};

class OmpService final : public settings::models::rpc::OmpRpc {
  public:
    explicit OmpService(OmpLaunch launch) : launch_(std::move(launch)) {}
    [[nodiscard]] const OmpLaunch& launch() const { return launch_; }
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<std::shared_ptr<OmpClient>>> client() const;
    [[nodiscard]] contracts::Awaitable<void> shutdown(std::chrono::milliseconds grace) const;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<std::vector<settings::models::rpc::LoginProvider>>> get_login_providers() const override;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<std::string>> login(std::string provider_id, std::shared_ptr<settings::models::rpc::LoginUiChannel> ui) const override;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<>> respond_ui(settings::models::rpc::UiResponse response) const override;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<std::vector<settings::models::rpc::ModelInfo>>> get_available_models() const override;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<settings::models::rpc::ModelInfo>> set_model(std::string provider, std::string model_id) const override;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<std::vector<settings::models::rpc::LogoutAccount>>> get_logout_accounts(std::string provider_id) const override;
    [[nodiscard]] contracts::Awaitable<settings::models::rpc::RpcResult<std::optional<std::string>>> logout(std::string provider_id, std::int64_t credential_id) const override;
  private:
    OmpLaunch launch_;
    mutable std::mutex mutex_;
    mutable std::shared_ptr<OmpClient> client_;
};

class OmpAccount final : public settings::models::login::OmpProviderAccount {
  public:
    OmpAccount(std::shared_ptr<settings::models::rpc::OmpRpc> rpc, std::shared_ptr<contracts::UrlOpener> opener, OmpLaunch launch)
        : rpc(std::move(rpc)), opener(std::move(opener)), launch(std::move(launch)) {}
    std::shared_ptr<settings::models::rpc::OmpRpc> rpc;
    std::shared_ptr<contracts::UrlOpener> opener;
    OmpLaunch launch;
    [[nodiscard]] std::string_view id() const override { return "omp"; }
    [[nodiscard]] std::string_view name() const override { return "omp"; }
    [[nodiscard]] contracts::Awaitable<contracts::SignInState> sign_in_state() const override;
    [[nodiscard]] contracts::Result<std::unique_ptr<contracts::LoginStrategy>> login(contracts::LoginKind) const override;
    [[nodiscard]] std::optional<contracts::CommandSpec> logout_command() const override { return std::nullopt; }
    [[nodiscard]] std::span<const std::string_view> efforts() const override;
    [[nodiscard]] std::optional<std::filesystem::path> config_path() const override;
    [[nodiscard]] contracts::Result<contracts::ModelSettings> read_model() const override;
    [[nodiscard]] contracts::Result<> write_model(const contracts::ModelSettings& settings) const override;
    [[nodiscard]] contracts::Result<std::unique_ptr<contracts::LoginStrategy>> login_provider(settings::models::rpc::LoginProvider provider) const override;
  private:
    [[nodiscard]] contracts::Result<std::filesystem::path> config_path_result() const;
};

} // namespace loa::harness::omp
