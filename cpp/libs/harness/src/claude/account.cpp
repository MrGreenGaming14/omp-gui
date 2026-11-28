#include "loa/harness/claude/account.hpp"

#include <asio/awaitable.hpp>

#include <algorithm>
#include <array>
#include <format>
#include <utility>

#include "loa/harness/account_support/account_support.hpp"
#include "loa/harness/account_support/cli.hpp"
#include "loa/harness/claude/driver.hpp"
#include "loa/runtime/json.hpp"

namespace loa::harness::claude {
namespace {
namespace json = loa::runtime::json;
using account_support::Error;
using account_support::JsonDoc;
using account_support::Result;
using loa::contracts::SignInMethod;
using loa::contracts::SignInState;

constexpr std::array<std::string_view, 4> EFFORTS{"low", "medium", "high", "xhigh"};
constexpr std::string_view MODEL_KEY = "model";
constexpr std::string_view EFFORT_KEY = "effortLevel";
constexpr std::string_view MODEL_SETTINGS_KEY = "modelSettings";

const json::Value* find(const json::Object& object, std::string_view key) {
    const auto it = std::ranges::find_if(object, [&](const auto& pair) { return pair.first == key; });
    return it == object.end() ? nullptr : &it->second;
}

std::optional<ClaudeEffort> effort_of(const json::Value& value) {
    const auto* field = value.find(EFFORT_KEY);
    return field && field->as_string() ? parse_effort(*field->as_string()) : std::nullopt;
}

bool only_key(const json::Object& object, std::string_view key) {
    return object.size() == 1 && object.front().first == key;
}

bool method_name(std::string_view name) {
    if (name.empty() || name.size() > 32) return false;
    return std::ranges::all_of(name, [](unsigned char c) {
        return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '.' || c == '_' || c == '-';
    });
}

SignInState parse_auth_status(std::optional<int> code, std::string_view stdout_text) {
    if (!code) return loa::contracts::SignInUnknown{"claude auth status was ended by a signal"};
    if (*code == 1) return loa::contracts::SignedOut{};
    if (*code != 0) return loa::contracts::SignInUnknown{std::format("claude auth status exited with code {}", *code)};
    auto status = json::parse(stdout_text);
    if (!status || !status->is_object())
        return loa::contracts::SignInUnknown{"claude auth status printed unexpected output"};
    const auto* logged_in = status->find("loggedIn");
    if (logged_in && logged_in->as_bool() && !*logged_in->as_bool()) return loa::contracts::SignedOut{};
    const auto* auth_method = status->find("authMethod");
    const std::string* method = auth_method ? auth_method->as_string() : nullptr;
    if (method && *method == "none") return loa::contracts::SignedOut{};
    SignInMethod parsed;
    if (method && *method == "claude.ai") parsed.kind = SignInMethod::Kind::Subscription;
    else if (method && *method == "api_key") parsed.kind = SignInMethod::Kind::ApiKey;
    else if (method && *method == "api_key_helper") parsed.kind = SignInMethod::Kind::ApiKeyHelper;
    else if (method && *method == "oauth_token") parsed.kind = SignInMethod::Kind::OAuthToken;
    else if (method && *method == "third_party") parsed.kind = SignInMethod::Kind::ThirdParty;
    else parsed.other = method && method_name(*method) ? *method : "unknown";
    return loa::contracts::SignedIn{std::move(parsed)};
}

loa::contracts::AdapterError account_error(const Error& error) {
    return loa::contracts::AdapterError{loa::contracts::AdapterError::Protocol{error.to_string()}};
}

} // namespace

std::optional<ClaudeEffort> parse_effort(std::string_view value) noexcept {
    for (std::size_t i = 0; i < EFFORTS.size(); ++i)
        if (value == EFFORTS[i]) return static_cast<ClaudeEffort>(i);
    return std::nullopt;
}

std::string_view effort_name(ClaudeEffort effort) noexcept { return EFFORTS[static_cast<std::size_t>(effort)]; }

account_support::Result<ClaudeModelSettings> ClaudeCode::read_model_settings() const {
    auto text = loa::settings::models::read_optional(settings_path_);
    if (!text) return std::unexpected(std::move(text.error()));
    auto doc = JsonDoc::parse(text->has_value() ? std::string_view(**text) : std::string_view{}, settings_path_.string());
    if (!doc) return std::unexpected(std::move(doc.error()));
    ClaudeModelSettings settings;
    settings.model = doc->get({MODEL_KEY});
    if (settings.model == "default") settings.model.reset();
    if (const auto* top = find(doc->root(), EFFORT_KEY); top && top->as_string())
        settings.effort = parse_effort(*top->as_string());
    if (const auto* models = find(doc->root(), MODEL_SETTINGS_KEY); models && models->as_object()) {
        for (const auto& [name, entry] : *models->as_object()) {
            if (auto level = effort_of(entry)) settings.per_model_effort.emplace(name, *level);
        }
    }
    return settings;
}

account_support::Result<> ClaudeCode::set_model(std::optional<std::string_view> model) const {
    if (model == "default") model.reset();
    if (model) {
        auto valid = account_support::validate_model_name(MODEL_KEY, *model);
        if (!valid) return std::unexpected(std::move(valid.error()));
    }
    return edit([model](JsonDoc& doc) { return doc.set({MODEL_KEY}, model); });
}

account_support::Result<> ClaudeCode::set_effort(std::optional<ClaudeEffort> effort) const {
    return edit([effort](JsonDoc& doc) {
        return doc.set({EFFORT_KEY}, effort ? std::optional<std::string_view>{effort_name(*effort)} : std::nullopt);
    });
}

account_support::Result<> ClaudeCode::set_model_effort(std::string_view model, std::optional<ClaudeEffort> effort) const {
    auto valid = account_support::validate_model_name("modelSettings model", model);
    if (!valid) return std::unexpected(std::move(valid.error()));
    return edit([model, effort](JsonDoc& doc) -> Result<> {
        if (!effort) {
            const auto* models = find(doc.root(), MODEL_SETTINGS_KEY);
            const auto* models_object = models ? models->as_object() : nullptr;
            const auto* entry = models_object ? find(*models_object, model) : nullptr;
            const auto* entry_object = entry ? entry->as_object() : nullptr;
            if (entry_object && only_key(*entry_object, EFFORT_KEY)) {
                if (only_key(*models_object, model)) return doc.set({MODEL_SETTINGS_KEY}, std::nullopt);
                return doc.set({MODEL_SETTINGS_KEY, model}, std::nullopt);
            }
        }
        return doc.set({MODEL_SETTINGS_KEY, model, EFFORT_KEY},
                       effort ? std::optional<std::string_view>{effort_name(*effort)} : std::nullopt);
    });
}

std::string_view ClaudeCode::id() const { return HARNESS; }

loa::contracts::CommandSpec ClaudeCode::command(std::vector<std::string> args) const {
    return loa::contracts::CommandSpec::make(program_, std::move(args));
}

loa::contracts::Awaitable<SignInState> ClaudeCode::sign_in_state() const {
    const auto spec = command({"auth", "status"});
    auto output = co_await runner_->output(spec);
    if (!output) {
        if (output.error().kind == Error::Kind::Process)
            co_return loa::contracts::SignInUnknown{std::format("cannot run {}: {}", spec.program_name(), output.error().message)};
        co_return loa::contracts::SignInUnknown{output.error().to_string()};
    }
    co_return parse_auth_status(output->code, output->out);
}

loa::contracts::Result<std::unique_ptr<loa::contracts::LoginStrategy>> ClaudeCode::login(loa::contracts::LoginKind kind) const {
    std::vector<std::string> args{"auth", "login"};
    switch (kind) {
    case loa::contracts::LoginKind::Browser: break;
    case loa::contracts::LoginKind::Console: args.emplace_back("--console"); break;
    case loa::contracts::LoginKind::Sso: args.emplace_back("--sso"); break;
    case loa::contracts::LoginKind::DeviceCode:
        return std::unexpected(account_error(Error::invalid("login kind", "Claude Code has no device code login")));
    }
    return std::make_unique<account_support::CliLoginCommand>(runner_, command(std::move(args)));
}

std::optional<loa::contracts::CommandSpec> ClaudeCode::logout_command() const { return command({"auth", "logout"}); }
std::span<const std::string_view> ClaudeCode::efforts() const { return EFFORTS; }

loa::contracts::Result<loa::contracts::ModelSettings> ClaudeCode::read_model() const {
    auto settings = read_model_settings();
    if (!settings) return std::unexpected(account_error(settings.error()));
    return loa::contracts::ModelSettings{std::move(settings->model),
                                         settings->effort ? std::optional<std::string>{effort_name(*settings->effort)} : std::nullopt};
}

loa::contracts::Result<> ClaudeCode::write_model(const loa::contracts::ModelSettings& settings) const {
    std::optional<ClaudeEffort> effort;
    if (settings.effort) {
        effort = parse_effort(*settings.effort);
        if (!effort)
            return std::unexpected(account_error(Error::invalid("effortLevel",
                std::format("\"{}\" is not one of low, medium, high, xhigh", *settings.effort))));
    }
    auto before = read_model_settings();
    if (!before) return std::unexpected(account_error(before.error()));
    if (before->model != settings.model) {
        auto result = set_model(settings.model ? std::optional<std::string_view>{*settings.model} : std::nullopt);
        if (!result) return std::unexpected(account_error(result.error()));
    }
    if (before->effort != effort) {
        auto result = set_effort(effort);
        if (!result) return std::unexpected(account_error(result.error()));
    }
    return {};
}

} // namespace loa::harness::claude
