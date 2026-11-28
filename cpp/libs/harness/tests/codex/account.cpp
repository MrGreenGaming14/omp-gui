#include <doctest/doctest.h>

#include <array>
#include <cstdlib>
#include <fstream>
#include <ranges>

#include "../kit/support.hpp"
#include "loa/contracts/login_channels.hpp"
#include "loa/harness/codex/account.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/tempfile.hpp"

using loa::contracts::LoginKind;
using loa::contracts::ModelSettings;
using loa::contracts::SignInMethod;
using loa::contracts::SignedIn;
using loa::contracts::SignedOut;
using loa::contracts::SignInUnknown;
using loa::harness::account_support::AsioProcessRunner;
using loa::harness::codex::Codex;
using loa::harness::codex::CodexModelSettings;
using loa::harness::codex::ReasoningEffort;
using loa::harness::test::block_on;
using loa::harness::test::read_file;

namespace {

std::filesystem::path fake_program(const std::filesystem::path& dir, std::string_view body) {
    const auto path = dir / "codex";
    REQUIRE(loa::runtime::exec::write_executable(path, "#!/bin/sh\n" + std::string(body) + "\n"));
    return loa::runtime::exec::resolve(path);
}

struct LoginRun {
    explicit LoginRun(loa::runtime::Executor& exec)
        : strand(asio::make_strand(exec.context())),
          channels(std::make_shared<loa::contracts::LoginChannels>(strand, 8)) {}

    std::future<void> start(std::unique_ptr<loa::contracts::LoginStrategy> strategy) {
        auto run = [](std::unique_ptr<loa::contracts::LoginStrategy> login,
                      std::shared_ptr<loa::contracts::LoginChannels> channels) -> asio::awaitable<void> {
            co_await login->run(std::move(channels));
        };
        return asio::co_spawn(strand, run(std::move(strategy), channels), asio::use_future);
    }

    loa::contracts::LoginUpdate next() {
        auto receive = [](std::shared_ptr<loa::contracts::LoginChannels> channels)
            -> asio::awaitable<loa::contracts::LoginUpdate> {
            co_return co_await channels->updates.async_receive(asio::use_awaitable);
        };
        return asio::co_spawn(strand, receive(channels), asio::use_future).get();
    }

    asio::strand<asio::io_context::executor_type> strand;
    std::shared_ptr<loa::contracts::LoginChannels> channels;
};

} // namespace

TEST_CASE("codex account: login status, commands and supported efforts") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto runner = std::make_shared<AsioProcessRunner>();
    const auto script = fake_program(dir.path(),
        "if [ \"$1 $2\" = 'login status' ]; then echo 'Logged in using ChatGPT'; exit 0; fi\n"
        "if [ \"$1 $2\" = 'login --device-auth' ]; then exit 7; fi\nexit 64");
    Codex account(script, dir.path() / "config.toml", runner);
    CHECK(account.id() == "codex");
    CHECK(account.name() == "Codex");
    CHECK(std::ranges::equal(account.efforts(), std::array<std::string_view, 6>{"none", "minimal", "low", "medium", "high", "xhigh"}));
    CHECK(block_on(exec, account.sign_in_state()) == loa::contracts::SignInState{SignedIn{{SignInMethod::Kind::Subscription, {}}}});
    REQUIRE(account.login(LoginKind::Browser));
    REQUIRE(account.login(LoginKind::DeviceCode));
    CHECK_FALSE(account.login(LoginKind::Console));
    CHECK_FALSE(account.login(LoginKind::Sso));
    CHECK(account.logout_command() == loa::contracts::CommandSpec::make(script, {"logout"}));

    const auto proc = block_on(exec, runner->spawn(loa::contracts::CommandSpec::make(script, {"login", "--device-auth"})));
    REQUIRE(proc);
    CHECK(block_on(exec, (*proc)->wait()).value() == 7);
}

TEST_CASE("codex account: browser and device-code login drive Codex CLI") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto program = fake_program(dir.path(),
        "if [ \"$1 $2\" = 'login --device-auth' ]; then exit 7; fi\n"
        "if [ \"$1\" = 'login' ] && [ -z \"$2\" ]; then echo 'Visit https://example.invalid/login'; exit 0; fi\nexit 64");
    Codex account(program, dir.path() / "config.toml", std::make_shared<AsioProcessRunner>());
    LoginRun browser(exec);
    auto login = account.login(LoginKind::Browser);
    REQUIRE(login);
    auto done = browser.start(std::move(*login));
    CHECK(browser.next() == loa::contracts::LoginUpdate{loa::contracts::login::Url{"https://example.invalid/login", std::nullopt, std::nullopt}});
    CHECK(browser.next() == loa::contracts::LoginUpdate{loa::contracts::login::Succeeded{std::nullopt}});
    done.get();

    LoginRun device(exec);
    auto device_login = account.login(LoginKind::DeviceCode);
    REQUIRE(device_login);
    auto device_done = device.start(std::move(*device_login));
    auto update = device.next();
    REQUIRE(std::holds_alternative<loa::contracts::login::Failed>(update));
    CHECK(std::get<loa::contracts::login::Failed>(update).reason.find("code 7") != std::string::npos);
    device_done.get();
}

TEST_CASE("codex account: status distinguishes credentials, signed out and failed commands") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto runner = std::make_shared<AsioProcessRunner>();
    const auto path = dir.path() / "config.toml";
    auto api = fake_program(dir.path(), "echo 'logged in with API KEY'; exit 0");
    CHECK(block_on(exec, Codex(api, path, runner).sign_in_state()) ==
          loa::contracts::SignInState{SignedIn{{SignInMethod::Kind::ApiKey, {}}}});
    fake_program(dir.path(), "exit 1");
    CHECK(block_on(exec, Codex(api, path, runner).sign_in_state()) == loa::contracts::SignInState{SignedOut{}});
    fake_program(dir.path(), "echo 'secret token'; exit 42");
    CHECK(block_on(exec, Codex(api, path, runner).sign_in_state()) ==
          loa::contracts::SignInState{SignInUnknown{"codex login status did not complete successfully"}});
    CHECK(block_on(exec, Codex(dir.path() / "missing", path, runner).sign_in_state()) ==
          loa::contracts::SignInState{SignInUnknown{"codex login status could not run"}});
}

TEST_CASE("codex account: model settings preserve unrelated TOML and comments") {
    auto dir = loa::runtime::TempDir::create().value();
    const auto path = dir.path() / "config.toml";
    const std::string original = "# keep this\nmodel = 'old' # user's note\nmodel_reasoning_effort = 'medium'\n"
                                 "[projects.'/work']\ntrust_level = 'trusted'\n";
    std::ofstream(path) << original;
    Codex account("codex", path, std::make_shared<AsioProcessRunner>());
    REQUIRE(account.set_model("gpt-5.4"));
    REQUIRE(account.set_reasoning_effort(ReasoningEffort::High));
    CHECK(account.read_model_settings().value() == CodexModelSettings{"gpt-5.4", ReasoningEffort::High});
    CHECK(account.read_model().value() == ModelSettings{"gpt-5.4", "high"});
    const auto contents = read_file(path);
    CHECK(contents.find("# keep this") != std::string::npos);
    CHECK(contents.find("[projects.'/work']\ntrust_level = 'trusted'\n") != std::string::npos);
    CHECK(contents.find("# user's note") != std::string::npos);
    REQUIRE(account.write_model(ModelSettings{"gpt-5.4", "high"}));
    CHECK(read_file(path) == contents);
    REQUIRE(account.write_model(ModelSettings{std::nullopt, std::nullopt}));
    CHECK(account.read_model().value() == ModelSettings{});
    CHECK(read_file(path).find("trust_level = 'trusted'") != std::string::npos);
}

TEST_CASE("codex account: missing config, unknown effort and invalid model") {
    auto dir = loa::runtime::TempDir::create().value();
    const auto path = dir.path() / "config.toml";
    Codex account("codex", path, std::make_shared<AsioProcessRunner>());
    CHECK(account.read_model().value() == ModelSettings{});
    CHECK_FALSE(std::filesystem::exists(path));
    CHECK_FALSE(account.write_model(ModelSettings{"gpt-5", "invalid"}));
    CHECK_FALSE(std::filesystem::exists(path));
    CHECK_FALSE(account.set_model("bad model"));
    REQUIRE(account.write_model(ModelSettings{"gpt-5", "xhigh"}));
    CHECK(account.read_model().value() == ModelSettings{"gpt-5", "xhigh"});
    std::ofstream(path) << "model_reasoning_effort = 'future'\n";
    CHECK(account.read_model_settings().value().reasoning_effort == std::nullopt);
    std::ofstream(path) << "model = [\n";
    CHECK_FALSE(account.read_model());
}

TEST_CASE("codex account: CODEX_HOME chooses the config file") {
    auto dir = loa::runtime::TempDir::create().value();
    const char* old = std::getenv("CODEX_HOME");
    const std::optional<std::string> previous = old ? std::optional<std::string>(old) : std::nullopt;
    struct Restore {
        std::optional<std::string> value;
        ~Restore() {
            if (value) ::setenv("CODEX_HOME", value->c_str(), 1);
            else ::unsetenv("CODEX_HOME");
        }
    } restore{previous};
    ::setenv("CODEX_HOME", dir.path().c_str(), 1);
    Codex account("codex", std::make_shared<AsioProcessRunner>());
    CHECK(account.config_path() == dir.path() / "config.toml");
    REQUIRE(account.set_model("gpt-5"));
    CHECK(read_file(dir.path() / "config.toml").find("model = \"gpt-5\"") != std::string::npos);
}
