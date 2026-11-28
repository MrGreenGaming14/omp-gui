#include <doctest/doctest.h>

#include <array>
#include <fstream>

#include "../kit/support.hpp"
#include "loa/contracts/login_channels.hpp"
#include "loa/harness/claude/account.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/tempfile.hpp"

using loa::harness::claude::ClaudeCode;
using loa::harness::claude::ClaudeEffort;
using loa::harness::claude::parse_effort;
using loa::harness::account_support::AsioProcessRunner;
using loa::harness::test::block_on;
using loa::harness::test::read_file;
using Kind = loa::settings::models::Error::Kind;
namespace contracts = loa::contracts;

namespace {
constexpr std::string_view SETTINGS = R"CLAUDE({
  "$schema": "https://json.schemastore.org/claude-code-settings.json",
  "permissions": {"allow": ["Read(~/.zshrc)"], "deny": ["Read(./.env)"]},
  "model": "sonnet",
  "hooks": {"PostToolUse": [{"hooks": [{"command": "echo preserved"}]}]},
  "modelSettings": {
    "opus": {"maxEffortLevel": "max", "autoCompactWindow": 400000},
    "haiku": {"effortLevel": "low"}
  },
  "includeCoAuthoredBy": false
}
)CLAUDE";

std::filesystem::path fake_program(const std::filesystem::path& dir, std::string_view body) {
    auto path = dir / "claude";
    REQUIRE(loa::runtime::exec::write_executable(path, "#!/bin/sh\n" + std::string(body) + "\n"));
    return loa::runtime::exec::resolve(path);
}

ClaudeCode account(const std::filesystem::path& program, const std::filesystem::path& path) {
    return ClaudeCode(program, path, std::make_shared<AsioProcessRunner>());
}

std::filesystem::path settings_file(const loa::runtime::TempDir& dir) {
    const auto path = dir.path() / "settings.json";
    std::ofstream(path) << SETTINGS;
    return path;
}

loa::runtime::json::Value json_file(const std::filesystem::path& path) {
    return loa::runtime::json::parse(read_file(path)).value();
}

} // namespace

TEST_CASE("claude account: reads settings and missing file") {
    auto dir = loa::runtime::TempDir::create().value();
    auto code = account("claude", settings_file(dir));
    const auto settings = code.read_model_settings().value();
    CHECK(settings.model == "sonnet");
    CHECK_FALSE(settings.effort);
    CHECK(settings.per_model_effort == std::map<std::string, ClaudeEffort>{{"haiku", ClaudeEffort::Low}});
    CHECK(code.read_model().value() == contracts::ModelSettings{std::string("sonnet"), std::nullopt});
    CHECK(account("claude", dir.path() / "absent.json").read_model_settings().value() == loa::harness::claude::ClaudeModelSettings{});
    CHECK(code.id() == "claude-code");
    CHECK(code.name() == "Claude Code");
    CHECK(code.config_path() == dir.path() / "settings.json");
    CHECK(std::vector<std::string_view>(code.efforts().begin(), code.efforts().end()) ==
          std::vector<std::string_view>{"low", "medium", "high", "xhigh"});
}

TEST_CASE("claude account: edits preserve other settings and key order") {
    auto dir = loa::runtime::TempDir::create().value();
    auto path = settings_file(dir);
    auto code = account("claude", path);
    auto expected = loa::runtime::json::parse(SETTINGS).value();
    REQUIRE(code.set_model("claude-opus-4-1[1m]"));
    expected.set("model", "claude-opus-4-1[1m]");
    CHECK(json_file(path) == expected);
    // JSON serialization must retain original key order (inspect rendered text directly).
    CHECK(read_file(path).find("\"permissions\"") < read_file(path).find("\"model\""));
    REQUIRE(code.set_model("default"));
    CHECK_FALSE(code.read_model_settings().value().model);
    REQUIRE(code.set_effort(ClaudeEffort::XHigh));
    CHECK(code.read_model_settings().value().effort == ClaudeEffort::XHigh);
    REQUIRE(code.set_effort(std::nullopt));
    REQUIRE(code.set_model_effort("opus", ClaudeEffort::High));
    CHECK(code.read_model_settings().value().per_model_effort.at("opus") == ClaudeEffort::High);
    REQUIRE(code.set_model_effort("opus", std::nullopt));
    CHECK(code.read_model_settings().value().per_model_effort.count("opus") == 0);
    REQUIRE(code.set_model_effort("haiku", std::nullopt));
    CHECK(code.read_model_settings().value().per_model_effort.empty());
    CHECK(read_file(path).find("maxEffortLevel") != std::string::npos);
    CHECK(read_file(path).find("includeCoAuthoredBy") != std::string::npos);
}

TEST_CASE("claude account: creates settings in absent config directory and prunes empty modelSettings") {
    auto dir = loa::runtime::TempDir::create().value();
    auto path = dir.path() / "claude" / "settings.json";
    auto code = account("claude", path);
    REQUIRE(code.set_model("opus"));
    CHECK(read_file(path) == "{\n  \"model\": \"opus\"\n}\n");
    REQUIRE(code.set_model_effort("sonnet[1m]", ClaudeEffort::Medium));
    CHECK(read_file(path) == "{\n  \"model\": \"opus\",\n  \"modelSettings\": {\n    \"sonnet[1m]\": {\n      \"effortLevel\": \"medium\"\n    }\n  }\n}\n");
    REQUIRE(code.set_model_effort("sonnet[1m]", std::nullopt));
    CHECK(read_file(path) == "{\n  \"model\": \"opus\"\n}\n");
    REQUIRE(code.set_model(std::nullopt));
    CHECK(read_file(path) == "{}\n");
}

TEST_CASE("claude account: ignores unrecognized settings and rejects invalid writes") {
    auto dir = loa::runtime::TempDir::create().value();
    auto path = dir.path() / "settings.json";
    std::ofstream(path) << R"({"model":"default","effortLevel":"max","modelSettings":{"x":{"effortLevel":"minimal"},"y":{"effortLevel":"high"}}})";
    auto code = account("claude", path);
    auto current = code.read_model_settings().value();
    CHECK_FALSE(current.model);
    CHECK_FALSE(current.effort);
    CHECK(current.per_model_effort == std::map<std::string, ClaudeEffort>{{"y", ClaudeEffort::High}});
    auto unchanged = read_file(path);
    for (std::string_view bad : {"", "opus 4", "opus\n", "a\x07"}) {
        CHECK(code.set_model(bad).error().kind == Kind::Invalid);
        CHECK(code.set_model_effort(bad, ClaudeEffort::Low).error().kind == Kind::Invalid);
    }
    CHECK(read_file(path) == unchanged);
    CHECK_FALSE(parse_effort("max"));
    CHECK_FALSE(parse_effort("HIGH"));
    CHECK(parse_effort("xhigh") == ClaudeEffort::XHigh);
    auto invalid = code.write_model({std::nullopt, "max"});
    REQUIRE_FALSE(invalid);
    CHECK(invalid.error().get<contracts::AdapterError::Protocol>() != nullptr);
    CHECK(read_file(path) == unchanged);
    std::ofstream(path) << "[\"model\"]\n";
    CHECK(code.set_model("opus").error().kind == Kind::Unsupported);
    CHECK(read_file(path) == "[\"model\"]\n");
}

TEST_CASE("claude account: HarnessAccount write_model updates only changed fields") {
    auto dir = loa::runtime::TempDir::create().value();
    auto path = settings_file(dir);
    auto code = account("claude", path);
    REQUIRE(code.write_model({std::string("sonnet"), std::nullopt}));
    CHECK(read_file(path) == SETTINGS);
    REQUIRE(code.write_model({std::string("opus"), std::string("high")}));
    CHECK(code.read_model().value() == contracts::ModelSettings{std::string("opus"), std::string("high")});
    CHECK(code.read_model_settings().value().per_model_effort.at("haiku") == ClaudeEffort::Low);
}

TEST_CASE("claude account: auth status maps methods and safe unknowns") {
    loa::runtime::Executor executor(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto path = dir.path() / "settings.json";
    auto check = [&](std::string_view json, int exit_code) {
        auto script = std::string("if [ \"$1 $2\" = \"auth status\" ]; then\ncat <<'EOF'\n") + std::string(json) +
                      "\nEOF\nexit " + std::to_string(exit_code) + "\nfi\nexit 64";
        auto code = account(fake_program(dir.path(), script), path);
        return block_on(executor, code.sign_in_state());
    };
    CHECK(check(R"({"loggedIn":true,"authMethod":"claude.ai","email":"secret"})", 0) ==
          contracts::SignInState{contracts::SignedIn{{contracts::SignInMethod::Kind::Subscription, ""}}});
    CHECK(check(R"({"authMethod":"api_key"})", 0) ==
          contracts::SignInState{contracts::SignedIn{{contracts::SignInMethod::Kind::ApiKey, ""}}});
    CHECK(check(R"({"authMethod":"api_key_helper"})", 0) ==
          contracts::SignInState{contracts::SignedIn{{contracts::SignInMethod::Kind::ApiKeyHelper, ""}}});
    CHECK(check(R"({"authMethod":"oauth_token"})", 0) ==
          contracts::SignInState{contracts::SignedIn{{contracts::SignInMethod::Kind::OAuthToken, ""}}});
    CHECK(check(R"({"authMethod":"third_party"})", 0) ==
          contracts::SignInState{contracts::SignedIn{{contracts::SignInMethod::Kind::ThirdParty, ""}}});
    CHECK(check(R"({"authMethod":"future-method"})", 0) ==
          contracts::SignInState{contracts::SignedIn{{contracts::SignInMethod::Kind::Other, "future-method"}}});
    CHECK(check(R"({"authMethod":"not safe secret"})", 0) ==
          contracts::SignInState{contracts::SignedIn{{contracts::SignInMethod::Kind::Other, "unknown"}}});
    CHECK(check(R"({"loggedIn":false,"authMethod":"api_key"})", 0) == contracts::SignInState{contracts::SignedOut{}});
    CHECK(check(R"({"authMethod":"none"})", 0) == contracts::SignInState{contracts::SignedOut{}});
    CHECK(check("{}", 1) == contracts::SignInState{contracts::SignedOut{}});
    CHECK(std::holds_alternative<contracts::SignInUnknown>(check("not json", 0)));
    CHECK(std::holds_alternative<contracts::SignInUnknown>(check("{}", 2)));
    auto missing = account(dir.path() / "missing", path);
    CHECK(std::holds_alternative<contracts::SignInUnknown>(block_on(executor, missing.sign_in_state())));
}

TEST_CASE("claude account: login kind commands and logout command") {
    loa::runtime::Executor executor(1);
    auto dir = loa::runtime::TempDir::create().value();
    const auto args_file = dir.path() / "args";
    auto script = std::string("printf '%s\\n' \"$@\" > '") + args_file.string() + "'\nexit 7";
    auto code = account(fake_program(dir.path(), script), dir.path() / "settings.json");
    for (const auto& [kind, expected] : std::array{
             std::pair{contracts::LoginKind::Browser, std::string_view{"auth\nlogin\n"}},
             std::pair{contracts::LoginKind::Console, std::string_view{"auth\nlogin\n--console\n"}},
             std::pair{contracts::LoginKind::Sso, std::string_view{"auth\nlogin\n--sso\n"}}}) {
        auto strategy = code.login(kind);
        REQUIRE(strategy);
        auto channels = std::make_shared<contracts::LoginChannels>(executor.context().get_executor(), 16);
        block_on(executor, (*strategy)->run(channels));
        CHECK(read_file(args_file) == expected);
    }
    auto bad = code.login(contracts::LoginKind::DeviceCode);
    REQUIRE_FALSE(bad);
    CHECK(bad.error().message().find("device code") != std::string::npos);
    CHECK(code.logout_command()->args == std::vector<std::string>{"auth", "logout"});
}
