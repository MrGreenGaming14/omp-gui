#pragma once

// The omp RPC port: the commands model settings need, shaped after omp's RPC protocol
// (`omp --mode rpc`), and the login UI frames omp sends while a login runs
// (port of `models/rpc.rs`).
//
// | Method | RPC command | Response `data` |
// | --- | --- | --- |
// | `get_login_providers` | `{"type":"get_login_providers"}` | `{ providers: LoginProvider[] }` |
// | `login` | `{"type":"login","providerId"}` | `{ providerId }`, with `extension_ui_request` frames while it runs |
// | `respond_ui` | `{"type":"extension_ui_response",...}` | none |
// | `get_available_models` | `{"type":"get_available_models"}` | `{ models: ModelInfo[] }` |
// | `set_model` | `{"type":"set_model","provider","modelId"}` | `ModelInfo` |
// | `get_logout_accounts` | `{"type":"get_logout_accounts","providerId"}` | `{ accounts: LogoutAccount[] }` |
// | `logout` | `{"type":"logout","providerId","credentialId"}` | `{ remainingSource? }` |

#include <cstdint>
#include <expected>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/runtime/error.hpp"
#include "loa/runtime/json.hpp"
#include "loa/settings/models/error.hpp"

namespace loa::settings::models::rpc {

using models::RpcError;

template <typename T = void>
using RpcResult = std::expected<T, RpcError>;

// An entry of `get_login_providers`: an OAuth or login-capable provider.
struct LoginProvider {
    std::string id;
    std::string name;
    // Whether omp offers a login for it now (paused providers are not).
    bool available = false;
    // Whether omp holds a credential for it.
    bool authenticated = false;
    friend bool operator==(const LoginProvider&, const LoginProvider&) = default;
};

// Per million token USD rates of a model.
struct ModelCost {
    double input = 0.0;
    double output = 0.0;
    double cache_read = 0.0;
    double cache_write = 0.0;
    friend bool operator==(const ModelCost&, const ModelCost&) = default;
};

// A model's thinking support as omp reports it.
struct ThinkingConfig {
    std::string mode;
    std::vector<std::string> efforts;
    std::optional<std::string> default_level;
    friend bool operator==(const ThinkingConfig&, const ThinkingConfig&) = default;
};

// A model as `get_available_models` and `set_model` return it (omp's `ModelInfo`).
// Fields the picker does not use are kept in `extra`, in document order.
struct ModelInfo {
    std::string id;
    std::string name;
    std::string provider;
    std::string api;
    bool reasoning = false;
    std::vector<std::string> input;
    ModelCost cost;
    std::optional<std::uint64_t> context_window;
    std::optional<std::uint64_t> max_tokens;
    std::optional<ThinkingConfig> thinking;
    runtime::json::Object extra;

    // The exact `provider/modelId` selector omp accepts everywhere.
    [[nodiscard]] std::string selector() const { return provider + "/" + id; }
    friend bool operator==(const ModelInfo&, const ModelInfo&) = default;
};

// UI frames omp sends while a `login` runs (`extension_ui_request`), tagged by `method`.
struct OpenUrl {
    std::string id;
    std::string url;
    // A short loopback redirect to `url`, the safer text to copy.
    std::optional<std::string> launch_url;
    std::optional<std::string> instructions;
    friend bool operator==(const OpenUrl&, const OpenUrl&) = default;
};
struct Notify {
    std::string id;
    std::string message;
    std::optional<std::string> notify_type;
    friend bool operator==(const Notify&, const Notify&) = default;
};
// A non-secret value the user must type.
struct Input {
    std::string id;
    std::string title;
    std::optional<std::string> placeholder;
    std::optional<std::uint64_t> timeout;
    friend bool operator==(const Input&, const Input&) = default;
};
// omp withdrew an earlier request (timeout or abort).
struct Cancel {
    std::string id;
    std::string target_id;
    friend bool operator==(const Cancel&, const Cancel&) = default;
};
using LoginUiRequest = std::variant<OpenUrl, Notify, Input, Cancel>;

// The host's answer to an `input` request (`extension_ui_response`), serialized untagged.
struct UiValue {
    std::string id;
    std::string value;
    friend bool operator==(const UiValue&, const UiValue&) = default;
};
struct UiCancelled {
    std::string id;
    bool cancelled = true;
    friend bool operator==(const UiCancelled&, const UiCancelled&) = default;
};
using UiResponse = std::variant<UiValue, UiCancelled>;

// The request a response answers.
[[nodiscard]] const std::string& response_id(const UiResponse& response) noexcept;

// A credential omp stores for a provider, as `get_logout_accounts` lists it.
struct LogoutAccount {
    std::int64_t credential_id = 0;
    std::string provider;
    // What the credential is, e.g. the account it signs in as.
    std::string label;
    std::string detail;
    // `oauth` or `api_key` (JSON key `type`).
    std::string kind;
    // Whether omp signs in with it now.
    bool active = false;
    friend bool operator==(const LogoutAccount&, const LogoutAccount&) = default;
};

// serde-compatible JSON forms (camelCase keys, as omp writes them).
[[nodiscard]] std::expected<LoginProvider, loa::Error> login_provider_from_json(const runtime::json::Value& v);
[[nodiscard]] runtime::json::Value to_json(const LoginProvider& v);
[[nodiscard]] std::expected<ModelCost, loa::Error> model_cost_from_json(const runtime::json::Value& v);
[[nodiscard]] runtime::json::Value to_json(const ModelCost& v);
[[nodiscard]] std::expected<ThinkingConfig, loa::Error> thinking_config_from_json(const runtime::json::Value& v);
[[nodiscard]] runtime::json::Value to_json(const ThinkingConfig& v);
[[nodiscard]] std::expected<ModelInfo, loa::Error> model_info_from_json(const runtime::json::Value& v);
[[nodiscard]] runtime::json::Value to_json(const ModelInfo& v);
[[nodiscard]] std::expected<LoginUiRequest, loa::Error> login_ui_request_from_json(const runtime::json::Value& v);
[[nodiscard]] runtime::json::Value to_json(const LoginUiRequest& v);
[[nodiscard]] std::expected<UiResponse, loa::Error> ui_response_from_json(const runtime::json::Value& v);
[[nodiscard]] runtime::json::Value to_json(const UiResponse& v);
[[nodiscard]] std::expected<LogoutAccount, loa::Error> logout_account_from_json(const runtime::json::Value& v);
[[nodiscard]] runtime::json::Value to_json(const LogoutAccount& v);

// Where `login` forwards `extension_ui_request` frames (tokio `UnboundedSender` in the
// Rust app). Defined in `rpc_channel.hpp`, which pulls in Asio.
struct LoginUiChannel;

// The omp RPC commands model settings use. The app backs it with its omp RPC client;
// tests back it with an in-memory fake.
class OmpRpc {
  public:
    virtual ~OmpRpc() = default;
    [[nodiscard]] virtual contracts::Awaitable<RpcResult<std::vector<LoginProvider>>> get_login_providers() const = 0;
    // `login` for `provider_id`. Every `extension_ui_request` frame is sent on `ui`.
    // Resolves with the provider id on success.
    [[nodiscard]] virtual contracts::Awaitable<RpcResult<std::string>> login(std::string provider_id,
                                                                            std::shared_ptr<LoginUiChannel> ui) const = 0;
    [[nodiscard]] virtual contracts::Awaitable<RpcResult<>> respond_ui(UiResponse response) const = 0;
    [[nodiscard]] virtual contracts::Awaitable<RpcResult<std::vector<ModelInfo>>> get_available_models() const = 0;
    // Switches the session's model; returns the selected model.
    [[nodiscard]] virtual contracts::Awaitable<RpcResult<ModelInfo>> set_model(std::string provider, std::string model_id) const = 0;
    // The credentials omp stores for `provider_id`, the active one first.
    [[nodiscard]] virtual contracts::Awaitable<RpcResult<std::vector<LogoutAccount>>> get_logout_accounts(std::string provider_id) const = 0;
    // Removes credential `credential_id`; resolves with the source omp still
    // authenticates the provider through, if any.
    [[nodiscard]] virtual contracts::Awaitable<RpcResult<std::optional<std::string>>> logout(std::string provider_id,
                                                                                           std::int64_t credential_id) const = 0;
};

} // namespace loa::settings::models::rpc
