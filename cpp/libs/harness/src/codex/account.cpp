#include "loa/harness/codex/account.hpp"

#include <algorithm>
#include <array>
#include <utility>

#include "loa/harness/account_support/account_support.hpp"
#include "loa/harness/account_support/cli.hpp"
#include "loa/harness/account_support/configdoc.hpp"
#include "loa/harness/codex/adapter.hpp"
#include "loa/harness/registry/registry.hpp"

namespace loa::harness::codex {
namespace {

using loa::contracts::AdapterError;
using loa::contracts::CommandSpec;
using loa::contracts::SignInMethod;
using loa::contracts::SignInState;
using loa::settings::models::Error;
using loa::settings::models::Result;

constexpr std::array<std::string_view, 6> effort_names{"none", "minimal", "low", "medium", "high", "xhigh"};

AdapterError account_error(const Error& error) { return AdapterError::Protocol{error.to_string()}; }

bool contains_ascii_insensitive(std::string_view text, std::string_view needle) {
    return std::search(text.begin(), text.end(), needle.begin(), needle.end(), [](char a, char b) {
        const auto lower = [](char c) { return c >= 'A' && c <= 'Z' ? static_cast<char>(c + ('a' - 'A')) : c; };
        return lower(a) == lower(b);
    }) != text.end();
}

SignInState status(std::optional<int> code, std::string_view text) {
    if (code == 1) return loa::contracts::SignedOut{};
    if (code == 0) {
        SignInMethod method;
        if (contains_ascii_insensitive(text, "chatgpt")) method.kind = SignInMethod::Kind::Subscription;
        else if (contains_ascii_insensitive(text, "api key")) method.kind = SignInMethod::Kind::ApiKey;
        else method.other = "unknown";
        return loa::contracts::SignedIn{std::move(method)};
    }
    return loa::contracts::SignInUnknown{"codex login status did not complete successfully"};
}

} // namespace

std::optional<ReasoningEffort> parse_reasoning_effort(std::string_view value) {
    for (std::size_t i = 0; i < effort_names.size(); ++i) {
        if (effort_names[i] == value) return static_cast<ReasoningEffort>(i);
    }
    return std::nullopt;
}

std::string_view reasoning_effort_name(ReasoningEffort effort) { return effort_names[static_cast<std::size_t>(effort)]; }

Codex::Codex(std::filesystem::path program, std::filesystem::path config_path,
             std::shared_ptr<const account_support::ProcessRunner> runner)
    : program_(std::move(program)), config_path_(std::move(config_path)), runner_(std::move(runner)) {}

Codex::Codex(std::filesystem::path program, std::shared_ptr<const account_support::ProcessRunner> runner)
    : Codex(std::move(program), registry::config_dir("CODEX_HOME", ".codex") / "config.toml", std::move(runner)) {}

CommandSpec Codex::command(std::vector<std::string> args) const { return CommandSpec::make(program_, std::move(args)); }

std::string_view Codex::id() const { return HARNESS; }
std::string_view Codex::name() const { return "Codex"; }

loa::contracts::Awaitable<SignInState> Codex::sign_in_state() const {
    auto output = co_await runner_->output(command({"login", "status"}));
    if (output) co_return status(output->code, output->out);
    if (output.error().kind == Error::Kind::Process)
        co_return loa::contracts::SignInUnknown{"codex login status could not run"};
    co_return loa::contracts::SignInUnknown{output.error().to_string()};
}

loa::contracts::Result<std::unique_ptr<loa::contracts::LoginStrategy>> Codex::login(loa::contracts::LoginKind kind) const {
    switch (kind) {
        case loa::contracts::LoginKind::Browser:
            return std::make_unique<account_support::CliLoginCommand>(runner_, command({"login"}));
        case loa::contracts::LoginKind::DeviceCode:
            return std::make_unique<account_support::CliLoginCommand>(runner_, command({"login", "--device-auth"}));
        default:
            return std::unexpected(account_error(Error::invalid("login kind", "Codex supports browser and device-code login")));
    }
}

std::optional<CommandSpec> Codex::logout_command() const { return command({"logout"}); }
std::span<const std::string_view> Codex::efforts() const { return effort_names; }
std::optional<std::filesystem::path> Codex::config_path() const { return config_path_; }

Result<CodexModelSettings> Codex::read_model_settings() const {
    auto loaded = loa::settings::models::load_snapshot(config_path_);
    if (!loaded) return std::unexpected(std::move(loaded.error()));
    auto doc = account_support::TomlDoc::parse(loaded->second ? std::string_view(*loaded->second) : std::string_view{}, config_path_.string());
    if (!doc) return std::unexpected(std::move(doc.error()));
    return CodexModelSettings{doc->get({"model"}),
                              doc->get({"model_reasoning_effort"}).and_then(parse_reasoning_effort)};
}

Result<> Codex::edit(std::string_view key, std::optional<std::string_view> setting) const {
    auto loaded = loa::settings::models::load_snapshot(config_path_);
    if (!loaded) return std::unexpected(std::move(loaded.error()));
    auto result = account_support::edit_file<account_support::TomlDoc>(
        loaded->first, loaded->second ? std::string_view(*loaded->second) : std::string_view{}, [key, setting](account_support::TomlDoc& doc) {
            return doc.set({key}, setting);
        });
    if (!result) return std::unexpected(std::move(result.error()));
    return {};
}

Result<> Codex::set_model(std::optional<std::string_view> model) const {
    if (model) {
        if (auto checked = account_support::validate_model_name("model", *model); !checked)
            return std::unexpected(std::move(checked.error()));
    }
    return edit("model", model);
}

Result<> Codex::set_reasoning_effort(std::optional<ReasoningEffort> effort) const {
    return edit("model_reasoning_effort", effort.transform(reasoning_effort_name));
}

loa::contracts::Result<loa::contracts::ModelSettings> Codex::read_model() const {
    auto settings = read_model_settings();
    if (!settings) return std::unexpected(account_error(settings.error()));
    return loa::contracts::ModelSettings{std::move(settings->model),
                                         settings->reasoning_effort.transform([](ReasoningEffort value) {
                                             return std::string(reasoning_effort_name(value));
                                         })};
}

loa::contracts::Result<> Codex::write_model(const loa::contracts::ModelSettings& settings) const {
    std::optional<ReasoningEffort> effort;
    if (settings.effort) {
        effort = parse_reasoning_effort(*settings.effort);
        if (!effort)
            return std::unexpected(account_error(Error::invalid("model_reasoning_effort", "unknown effort")));
    }
    auto before = read_model_settings();
    if (!before) return std::unexpected(account_error(before.error()));
    if (before->model != settings.model) {
        const std::optional<std::string_view> model = settings.model ? std::optional<std::string_view>(*settings.model) : std::nullopt;
        if (auto result = set_model(model); !result) return std::unexpected(account_error(result.error()));
    }
    if (before->reasoning_effort != effort) {
        if (auto result = set_reasoning_effort(effort); !result) return std::unexpected(account_error(result.error()));
    }
    return {};
}

} // namespace loa::harness::codex
