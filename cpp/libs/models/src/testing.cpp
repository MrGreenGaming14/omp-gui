#include "loa/models/testing.hpp"

#include <algorithm>

#include <asio/as_tuple.hpp>
#include <asio/experimental/channel.hpp>
#include <asio/steady_timer.hpp>
#include <asio/this_coro.hpp>
#include <asio/use_awaitable.hpp>

#include "loa/settings/models/rpc_channel.hpp"

namespace loa::models::testing {

namespace {
template <typename T>
using Awaitable = contracts::Awaitable<T>;
constexpr auto tuple_awaitable = asio::as_tuple(asio::use_awaitable);
using contracts::AdapterError;
} // namespace

struct FakeOmp::State {
    mutable std::mutex mutex;
    std::vector<rpc::UiResponse> responses;
    std::vector<std::pair<std::string, std::string>> selected;
    std::vector<rpc::LogoutAccount> credentials;
    std::set<std::string> authenticated;
    // Providers whose last stored credential was removed.
    std::set<std::string> signed_out;
    // tokio `Notify`: a stored permit, or the waiting login's wake-up channel.
    bool permit = false;
    std::shared_ptr<asio::experimental::channel<void(asio::error_code)>> answered;
};

FakeOmp::FakeOmp() : state_(std::make_unique<State>()) {}
FakeOmp::~FakeOmp() = default;

void FakeOmp::set_credentials(std::vector<rpc::LogoutAccount> credentials) {
    std::lock_guard guard(state_->mutex);
    state_->credentials = std::move(credentials);
}

std::vector<rpc::UiResponse> FakeOmp::responses() const {
    std::lock_guard guard(state_->mutex);
    return state_->responses;
}

std::vector<std::pair<std::string, std::string>> FakeOmp::selected() const {
    std::lock_guard guard(state_->mutex);
    return state_->selected;
}

Awaitable<rpc::RpcResult<std::vector<rpc::LoginProvider>>> FakeOmp::get_login_providers() const {
    std::vector<rpc::LoginProvider> out;
    {
        std::lock_guard guard(state_->mutex);
        for (auto p : providers) {
            p.authenticated = (p.authenticated || state_->authenticated.contains(p.id)) && !state_->signed_out.contains(p.id);
            out.push_back(std::move(p));
        }
    }
    co_return out;
}

Awaitable<rpc::RpcResult<std::string>> FakeOmp::login(std::string provider_id, std::shared_ptr<rpc::LoginUiChannel> ui) const {
    for (const auto& frame : login_frames) {
        if (!ui->channel.try_send(asio::error_code{}, frame)) co_return std::unexpected(rpc::RpcError::transport("channel closed"));
    }
    auto executor = co_await asio::this_coro::executor;
    if (std::holds_alternative<script::Hang>(script)) {
        asio::steady_timer forever(executor, asio::steady_timer::time_point::max());
        co_await forever.async_wait(tuple_awaitable);
        co_return std::unexpected(rpc::RpcError::transport("cancelled"));
    }
    if (const auto* result = std::get_if<script::Result>(&script)) co_return result->result;
    if (const auto* after = std::get_if<script::CompleteAfter>(&script)) {
        asio::steady_timer timer(executor, after->delay);
        co_await timer.async_wait(tuple_awaitable);
        std::lock_guard guard(state_->mutex);
        state_->authenticated.insert(provider_id);
        state_->signed_out.erase(provider_id);
        co_return provider_id;
    }
    // AwaitResponse / AwaitInput: wait for a UI response.
    std::shared_ptr<asio::experimental::channel<void(asio::error_code)>> answered;
    {
        std::lock_guard guard(state_->mutex);
        if (state_->permit) {
            state_->permit = false;
        } else {
            answered = std::make_shared<asio::experimental::channel<void(asio::error_code)>>(executor, 1);
            state_->answered = answered;
        }
    }
    if (answered) co_await answered->async_receive(tuple_awaitable);
    if (const auto* input = std::get_if<script::AwaitInput>(&script)) {
        std::lock_guard guard(state_->mutex);
        const auto* last = state_->responses.empty() ? nullptr : std::get_if<rpc::UiValue>(&state_->responses.back());
        if (!last || last->value != input->expected) co_return std::unexpected(rpc::RpcError::command_failed("login", "bad callback"));
    }
    co_return provider_id;
}

Awaitable<rpc::RpcResult<>> FakeOmp::respond_ui(rpc::UiResponse response) const {
    std::lock_guard guard(state_->mutex);
    state_->responses.push_back(std::move(response));
    if (state_->answered) {
        (void)state_->answered->try_send(asio::error_code{});
        state_->answered.reset();
    } else {
        state_->permit = true;
    }
    co_return rpc::RpcResult<>{};
}

Awaitable<rpc::RpcResult<std::vector<rpc::ModelInfo>>> FakeOmp::get_available_models() const { co_return models; }

Awaitable<rpc::RpcResult<rpc::ModelInfo>> FakeOmp::set_model(std::string provider, std::string model_id) const {
    {
        std::lock_guard guard(state_->mutex);
        state_->selected.emplace_back(provider, model_id);
    }
    const auto it = std::ranges::find_if(models, [&](const rpc::ModelInfo& m) { return m.provider == provider && m.id == model_id; });
    if (it == models.end()) {
        co_return std::unexpected(rpc::RpcError::command_failed("set_model", "Model not found: " + provider + "/" + model_id));
    }
    co_return *it;
}

Awaitable<rpc::RpcResult<std::vector<rpc::LogoutAccount>>> FakeOmp::get_logout_accounts(std::string provider_id) const {
    std::vector<rpc::LogoutAccount> out;
    std::lock_guard guard(state_->mutex);
    for (const auto& a : state_->credentials) {
        if (a.provider == provider_id) out.push_back(a);
    }
    co_return out;
}

Awaitable<rpc::RpcResult<std::optional<std::string>>> FakeOmp::logout(std::string provider_id, std::int64_t credential_id) const {
    std::lock_guard guard(state_->mutex);
    auto& credentials = state_->credentials;
    const auto it = std::ranges::find_if(credentials, [&](const rpc::LogoutAccount& a) {
        return a.provider == provider_id && a.credential_id == credential_id;
    });
    if (it == credentials.end()) {
        co_return std::unexpected(rpc::RpcError::command_failed(
            "logout", "Credential " + std::to_string(credential_id) + " is not stored for " + provider_id));
    }
    credentials.erase(it);
    if (std::ranges::none_of(credentials, [&](const rpc::LogoutAccount& a) { return a.provider == provider_id; })) {
        state_->authenticated.erase(provider_id);
        state_->signed_out.insert(provider_id);
    }
    co_return remaining_source;
}

Awaitable<contracts::SignInState> OfflineOmpAccount::sign_in_state() const { co_return contracts::SignedOut{}; }

contracts::Result<std::unique_ptr<LoginStrategy>> OfflineOmpAccount::login(contracts::LoginKind) const {
    return std::unexpected(AdapterError(AdapterError::Unsupported{"omp", "login without a provider"}));
}

std::span<const std::string_view> OfflineOmpAccount::efforts() const { return settings::models::selector::THINKING_LEVELS; }

contracts::Result<> OfflineOmpAccount::write_model(const contracts::ModelSettings&) const {
    return std::unexpected(AdapterError(AdapterError::Unsupported{"omp", "model settings offline"}));
}

contracts::Result<std::unique_ptr<LoginStrategy>> OfflineOmpAccount::login_provider(rpc::LoginProvider) const {
    return std::unexpected(AdapterError(AdapterError::Unsupported{"omp", "provider login offline"}));
}

Awaitable<settings::models::Result<>> RejectingValidator::check_config(std::string text,
                                                                       std::vector<std::pair<std::string, runtime::json::Value>>) const {
    if (text.find("reject-me") != std::string::npos) {
        co_return std::unexpected(settings::models::Error::rejected("config.yml", "bad role"));
    }
    co_return settings::models::Result<>{};
}

Awaitable<settings::models::Result<>> RejectingValidator::check_models(std::string) const { co_return settings::models::Result<>{}; }

std::expected<void, std::string> NoBrowser::open_url(std::string_view) const { return std::unexpected("no browser in tests"); }

bool NoEnv::contains(std::string_view) const { return false; }

} // namespace loa::models::testing
