#include "loa/harness/omp/account.hpp"

#include <algorithm>
#include <asio.hpp>
#include <utility>

#include "loa/harness/omp/oauth.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/settings/models/config.hpp"
#include "loa/settings/models/selector.hpp"

namespace loa::harness::omp {
namespace m = settings::models;
namespace sr = settings::models::rpc;
namespace wire = loa::harness::omp::rpc;
namespace {
std::string trim(std::string s) {
    auto first = s.find_first_not_of(" \t\r\n");
    if (first == std::string::npos) return {};
    return s.substr(first, s.find_last_not_of(" \t\r\n") - first + 1);
}
sr::RpcError error(const kit::RpcError& e) {
    if (auto* command = e.get<kit::RpcError::Command>())
        return sr::RpcError::command_failed(command->command, command->message, command->code);
    return sr::RpcError::transport(e.message());
}
template <typename T, typename F>
sr::RpcResult<std::vector<T>> decode_list(const wire::Json& data, std::string_view key, F decode) {
    const auto* value = data.find(key);
    if (!value || !value->as_array()) return std::unexpected(sr::RpcError::transport("missing " + std::string(key) + " in omp response"));
    std::vector<T> list;
    list.reserve(value->as_array()->size());
    for (const auto& entry : *value->as_array()) {
        auto item = decode(entry);
        if (!item) return std::unexpected(sr::RpcError::transport(item.error().message()));
        list.push_back(std::move(*item));
    }
    return list;
}
std::optional<sr::LoginUiRequest> login_frame(const wire::UiRequest& frame) {
    if (auto* v = std::get_if<wire::ui::OpenUrl>(&frame)) return sr::OpenUrl{v->id, v->url, v->launch_url, v->instructions};
    if (auto* v = std::get_if<wire::ui::Notify>(&frame)) return sr::Notify{v->id, v->message, v->notify_type};
    if (auto* v = std::get_if<wire::ui::Input>(&frame); v && !v->secret) return sr::Input{v->id, v->title, v->placeholder, v->timeout};
    if (auto* v = std::get_if<wire::ui::Cancel>(&frame)) return sr::Cancel{v->id, v->target_id};
    return std::nullopt;
}
contracts::AdapterError config_error(const m::Error& e) { return contracts::AdapterError::Protocol{e.to_string()}; }
} // namespace

struct LoginSlot {
    std::mutex mutex;
    std::weak_ptr<sr::LoginUiChannel> channel;
};

contracts::Awaitable<m::Result<m::files::OmpFiles>> OmpLaunch::resolve() const {
    auto out = runtime::exec::output(runtime::exec::command(executable, {"config", "path"}, env, cwd));
    if (!out) co_return std::unexpected(m::Error::process(executable.string(), out.error().message()));
    auto dir = trim(out->out);
    if (!out->status.success() || dir.empty())
        co_return std::unexpected(m::Error::process(executable.string(), "`config path` failed: " + trim(out->err)));
    co_return m::files::OmpFiles{dir};
}

contracts::Awaitable<void> OmpClient::forward(wire::Events events, wire::UiResponder responder, std::shared_ptr<LoginSlot> slot) {
    while (auto incoming = co_await events.recv()) {
        auto* request = std::get_if<wire::incoming::Ui>(&*incoming);
        if (!request) continue;
        std::shared_ptr<sr::LoginUiChannel> ui;
        {
            std::lock_guard lock(slot->mutex);
            ui = slot->channel.lock();
        }
        auto frame = login_frame(request->request);
        const bool dialog = wire::is_dialog(request->request);
        auto dismiss = [responder, id = std::string(wire::ui_id(request->request))] {
            (void)responder.respond(wire::UiResponse{wire::ui_response::Cancelled{id, false}});
        };
        if (!frame || !ui) {
            if (dialog) dismiss();
            continue;
        }
        // The login's channel lives on its own executor; Asio channels are not thread-safe.
        asio::post(ui->channel.get_executor(), [ui, frame = std::move(*frame), dialog, dismiss]() mutable {
            if (!ui->channel.try_send(asio::error_code{}, std::move(frame)) && dialog) dismiss();
        });
    }
}

contracts::Awaitable<sr::RpcResult<std::shared_ptr<OmpClient>>> OmpClient::spawn(const OmpLaunch& launch) {
    wire::SpawnOptions options(launch.executable);
    options.arg("--no-session");
    options.cwd = launch.cwd;
    options.env = launch.env;
    auto spawned = co_await wire::RpcClient::spawn(std::move(options));
    if (!spawned) co_return std::unexpected(error(spawned.error()));
    auto client = std::shared_ptr<OmpClient>(new OmpClient(std::move(spawned->client)));
    client->login_ = std::make_shared<LoginSlot>();
    asio::co_spawn(co_await asio::this_coro::executor,
                   forward(std::move(spawned->events), client->client_.ui_responder(), client->login_), asio::detached);
    co_return client;
}
OmpClient::~OmpClient() { client_.kill(); }
bool OmpClient::has_exited() const { return client_.has_exited(); }
contracts::Awaitable<void> OmpClient::shutdown(std::chrono::milliseconds grace) const {
    (void)co_await client_.shutdown(grace);
}
contracts::Awaitable<sr::RpcResult<std::optional<sr::ModelInfo>>> OmpClient::current_model() const {
    auto result = co_await client_.request(wire::command::GetState{});
    if (!result) co_return std::unexpected(error(result.error()));
    const auto* model = result->find("model");
    if (!model || model->is_null()) co_return std::nullopt;
    auto decoded = sr::model_info_from_json(*model);
    if (!decoded) co_return std::unexpected(sr::RpcError::transport(decoded.error().message()));
    co_return std::move(*decoded);
}
contracts::Awaitable<sr::RpcResult<std::vector<sr::LoginProvider>>> OmpClient::get_login_providers() const {
    auto result = co_await client_.request(wire::command::GetLoginProviders{});
    if (!result) co_return std::unexpected(error(result.error()));
    co_return decode_list<sr::LoginProvider>(*result, "providers", sr::login_provider_from_json);
}
contracts::Awaitable<sr::RpcResult<std::string>> OmpClient::login(std::string provider_id, std::shared_ptr<sr::LoginUiChannel> ui) const {
    {
        std::lock_guard lock(login_->mutex);
        if (auto running = login_->channel.lock(); running && running->channel.is_open())
            co_return std::unexpected(sr::RpcError::command_failed("login", "another login is already running"));
        login_->channel = ui;
    }
    auto result = co_await client_.request(wire::command::Login{provider_id});
    {
        std::lock_guard lock(login_->mutex);
        login_->channel.reset();
    }
    if (!result) co_return std::unexpected(error(result.error()));
    co_return provider_id;
}
contracts::Awaitable<sr::RpcResult<>> OmpClient::respond_ui(sr::UiResponse response) const {
    wire::UiResponse out = std::visit([](const auto& v) -> wire::UiResponse {
        using V = std::decay_t<decltype(v)>;
        if constexpr (std::is_same_v<V, sr::UiValue>) return wire::ui_response::Value{v.id, v.value};
        else return wire::ui_response::Cancelled{v.id, false};
    }, response);
    auto sent = client_.respond_ui(out);
    if (!sent) co_return std::unexpected(error(sent.error()));
    co_return sr::RpcResult<>{};
}
contracts::Awaitable<sr::RpcResult<std::vector<sr::ModelInfo>>> OmpClient::get_available_models() const {
    auto result = co_await client_.request(wire::command::GetAvailableModels{});
    if (!result) co_return std::unexpected(error(result.error()));
    co_return decode_list<sr::ModelInfo>(*result, "models", sr::model_info_from_json);
}
contracts::Awaitable<sr::RpcResult<sr::ModelInfo>> OmpClient::set_model(std::string provider, std::string model_id) const {
    auto result = co_await client_.request(wire::command::SetModel{std::move(provider), std::move(model_id)});
    if (!result) co_return std::unexpected(error(result.error()));
    auto model = sr::model_info_from_json(*result);
    if (!model) co_return std::unexpected(sr::RpcError::transport(model.error().message()));
    co_return std::move(*model);
}
contracts::Awaitable<sr::RpcResult<std::vector<sr::LogoutAccount>>> OmpClient::get_logout_accounts(std::string provider_id) const {
    auto result = co_await client_.request(wire::command::GetLogoutAccounts{std::move(provider_id)});
    if (!result) co_return std::unexpected(error(result.error()));
    co_return decode_list<sr::LogoutAccount>(*result, "accounts", sr::logout_account_from_json);
}
contracts::Awaitable<sr::RpcResult<std::optional<std::string>>> OmpClient::logout(std::string provider_id, std::int64_t credential_id) const {
    auto result = co_await client_.request(wire::command::Logout{std::move(provider_id), credential_id});
    if (!result) co_return std::unexpected(error(result.error()));
    const auto* source = result->find("remainingSource");
    if (!source || source->is_null()) co_return std::nullopt;
    if (!source->as_string()) co_return std::unexpected(sr::RpcError::transport("invalid remainingSource in omp response"));
    co_return *source->as_string();
}

contracts::Awaitable<sr::RpcResult<std::shared_ptr<OmpClient>>> OmpService::client() const {
    {
        std::lock_guard lock(mutex_);
        if (client_ && !client_->has_exited()) co_return client_;
    }
    auto started = co_await OmpClient::spawn(launch_);
    if (!started) co_return std::unexpected(started.error());
    std::lock_guard lock(mutex_);
    if (!client_ || client_->has_exited()) client_ = *started;
    co_return client_;
}
contracts::Awaitable<void> OmpService::shutdown(std::chrono::milliseconds grace) const {
    std::shared_ptr<OmpClient> running;
    {
        std::lock_guard lock(mutex_);
        running = std::move(client_);
    }
    if (running) co_await running->shutdown(grace);
}
#define DELEGATE(method, type, args, pass) \
contracts::Awaitable<sr::RpcResult<type>> OmpService::method args const { \
    auto running = co_await client(); \
    if (!running) co_return std::unexpected(running.error()); \
    co_return co_await (*running)->method pass; \
}
DELEGATE(get_login_providers, std::vector<sr::LoginProvider>, (), ())
DELEGATE(login, std::string, (std::string id, std::shared_ptr<sr::LoginUiChannel> ui), (std::move(id), std::move(ui)))
DELEGATE(respond_ui, void, (sr::UiResponse response), (std::move(response)))
DELEGATE(get_available_models, std::vector<sr::ModelInfo>, (), ())
DELEGATE(set_model, sr::ModelInfo, (std::string provider, std::string model_id), (std::move(provider), std::move(model_id)))
DELEGATE(get_logout_accounts, std::vector<sr::LogoutAccount>, (std::string id), (std::move(id)))
DELEGATE(logout, std::optional<std::string>, (std::string id, std::int64_t credential_id), (std::move(id), credential_id))
#undef DELEGATE

contracts::Result<std::filesystem::path> OmpAccount::config_path_result() const {
    auto out = runtime::exec::output(runtime::exec::command(launch.executable, {"config", "path"}, launch.env, launch.cwd));
    if (!out) return std::unexpected(contracts::AdapterError::Io{out.error()});
    auto dir = trim(out->out);
    if (!out->status.success() || dir.empty()) return std::unexpected(contracts::AdapterError::Protocol{"omp config path failed"});
    return std::filesystem::path(dir) / "config.yml";
}
contracts::Awaitable<contracts::SignInState> OmpAccount::sign_in_state() const {
    auto providers = co_await rpc->get_login_providers();
    if (!providers) co_return contracts::SignInUnknown{providers.error().to_string()};
    if (std::ranges::any_of(*providers, [](const sr::LoginProvider& provider) { return provider.authenticated; }))
        co_return contracts::SignedIn{contracts::SignInMethod{contracts::SignInMethod::Kind::OAuthToken, {}}};
    co_return contracts::SignedOut{};
}
contracts::Result<std::unique_ptr<contracts::LoginStrategy>> OmpAccount::login(contracts::LoginKind) const {
    return std::unexpected(contracts::AdapterError::Unsupported{"omp", "login without a provider"});
}
std::span<const std::string_view> OmpAccount::efforts() const { return m::selector::THINKING_LEVELS; }
std::optional<std::filesystem::path> OmpAccount::config_path() const { auto path = config_path_result(); return path ? std::optional(*path) : std::nullopt; }
contracts::Result<contracts::ModelSettings> OmpAccount::read_model() const {
    auto path = config_path_result();
    if (!path) return std::unexpected(path.error());
    auto config = m::config::OmpConfig::load(*path);
    if (!config) return std::unexpected(config_error(config.error()));
    auto role = config->model_role("default");
    if (!role) return std::unexpected(config_error(role.error()));
    if (!*role || (*role)->candidates.empty()) return contracts::ModelSettings{};
    const auto& selector = (*role)->candidates.front();
    return contracts::ModelSettings{selector.is_role() ? "@" + selector.name() : selector.name(), selector.thinking()};
}
contracts::Result<> OmpAccount::write_model(const contracts::ModelSettings& settings) const {
    auto path = config_path_result();
    if (!path) return std::unexpected(path.error());
    auto config = m::config::OmpConfig::load(*path);
    if (!config) return std::unexpected(config_error(config.error()));
    if (settings.model) {
        auto selector = m::selector::Selector::parse(*settings.model);
        if (!selector) return std::unexpected(config_error(selector.error()));
        auto role = config->model_role("default");
        if (!role) return std::unexpected(config_error(role.error()));
        auto candidates = *role ? std::move((*role)->candidates) : std::vector<m::selector::Selector>{};
        auto choice = selector->is_role() ? m::selector::Selector::role(selector->name(), settings.effort)
                                          : m::selector::Selector::model(selector->name(), settings.effort);
        if (candidates.empty()) candidates.push_back(std::move(choice));
        else candidates.front() = std::move(choice);
        auto set = config->set_model_role("default", candidates);
        if (!set) return std::unexpected(config_error(set.error()));
    } else {
        auto clear = config->clear_model_role("default");
        if (!clear) return std::unexpected(config_error(clear.error()));
    }
    auto saved = config->save();
    if (!saved) return std::unexpected(config_error(saved.error()));
    return {};
}
contracts::Result<std::unique_ptr<contracts::LoginStrategy>> OmpAccount::login_provider(sr::LoginProvider provider) const {
    return std::make_unique<OmpOAuthLogin>(rpc, opener, std::move(provider));
}
} // namespace loa::harness::omp
