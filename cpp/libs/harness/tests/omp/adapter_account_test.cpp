// omp account: the account-only RPC process, OAuth login, logout, model role edits and
// settings validation (port of the tests in `omp/{account,oauth,validate}.rs`).
#include <doctest/doctest.h>

#include <asio/experimental/awaitable_operators.hpp>

#include <chrono>
#include <fstream>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include "../kit/support.hpp"
#include "loa/contracts/login_channels.hpp"
#include "loa/harness/omp/account.hpp"
#include "loa/harness/omp/oauth.hpp"
#include "loa/harness/omp/validate.hpp"
#include "loa/harness/testing/testing.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::harness::omp;
using namespace loa::contracts;
namespace models = loa::settings::models;
namespace mrpc = loa::settings::models::rpc;
using loa::harness::test::block_on;
using loa::harness::test::read_file;
using loa::harness::testing::FakeProcess;
using loa::harness::testing::handshake_v2;
namespace json = loa::runtime::json;
namespace fs = std::filesystem;

namespace {

class Opener final : public UrlOpener {
  public:
    std::expected<void, std::string> open_url(std::string_view url) const override {
        std::scoped_lock lock(mutex);
        opened.emplace_back(url);
        return {};
    }
    mutable std::mutex mutex;
    mutable std::vector<std::string> opened;
};

OmpLaunch launch_of(const FakeProcess& fake) {
    OmpLaunch launch;
    launch.executable = fake.executable();
    return launch;
}

std::string str(const json::Value& frame, std::string_view key) {
    const auto* value = frame.find(key);
    return value && value->as_string() ? *value->as_string() : std::string();
}

// Runs `aw` on a strand of `exec`: login channels are Asio channels, which are not thread-safe.
template <typename T>
T block_on_strand(loa::runtime::Executor& exec, asio::awaitable<T> aw) {
    return asio::co_spawn(asio::make_strand(exec.context()), std::move(aw), asio::use_future).get();
}

// Runs `strategy` and answers its prompts with `input`; true when it reported success.
Awaitable<bool> drive(LoginStrategy& strategy, std::string input, std::vector<LoginUpdate>& seen, bool cancel = false) {
    using namespace asio::experimental::awaitable_operators;
    auto channels = std::make_shared<LoginChannels>(co_await asio::this_coro::executor, 16);
    auto answer = [](std::shared_ptr<LoginChannels> channels, std::string input, std::vector<LoginUpdate>& seen, bool cancel) -> Awaitable<bool> {
        for (;;) {
            auto [ec, update] = co_await channels->updates.async_receive(asio::as_tuple(asio::use_awaitable));
            if (ec) co_return false;
            seen.push_back(update);
            if (std::holds_alternative<login::Prompt>(update)) {
                LoginControl control = cancel ? LoginControl{CancelLogin{}} : LoginControl{SubmitInput{input}};
                co_await channels->control.async_send(asio::error_code{}, std::move(control), asio::use_awaitable);
            } else if (std::holds_alternative<login::Succeeded>(update)) {
                co_return true;
            } else if (std::holds_alternative<login::Failed>(update) || std::holds_alternative<login::Cancelled>(update)) {
                co_return false;
            }
        }
    };
    auto run = [](LoginStrategy& strategy, std::shared_ptr<LoginChannels> channels) -> Awaitable<void> {
        co_await strategy.run(channels);
    };
    co_return co_await (run(strategy, channels) && answer(channels, std::move(input), seen, cancel));
}

} // namespace

TEST_CASE("omp adapter: login frames reach the flow and input answers reach omp") {
    loa::runtime::Executor exec(3);
    auto dir = loa::runtime::TempDir::create().value();
    const std::string script = handshake_v2() +
        "< login lg\n"
        "> {\"type\":\"extension_ui_request\",\"id\":\"u1\",\"method\":\"open_url\",\"url\":\"https://auth.example/authorize\",\"launchUrl\":\"http://127.0.0.1:9/l\"}\n"
        "> {\"type\":\"extension_ui_request\",\"id\":\"u2\",\"method\":\"input\",\"title\":\"Paste the callback URL\"}\n"
        "< extension_ui_response\n"
        "> {\"type\":\"response\",\"id\":\"{{lg}}\",\"command\":\"login\",\"success\":true}\n";
    auto fake = FakeProcess::install(dir.path(), script).value();
    auto client = block_on(exec, OmpClient::spawn(launch_of(fake)));
    REQUIRE(client);
    CHECK(fake.args() == std::vector<std::string>{"--mode", "rpc", "--no-session"});
    auto opener = std::make_shared<Opener>();
    OmpOAuthLogin strategy(*client, opener, mrpc::LoginProvider{"anthropic", "Anthropic", true, false});
    std::vector<LoginUpdate> seen;
    CHECK(block_on_strand(exec, drive(strategy, "http://127.0.0.1:9/cb?code=1", seen)));
    CHECK(opener->opened == std::vector<std::string>{"https://auth.example/authorize"});
    REQUIRE_FALSE(seen.empty());
    CHECK(seen.front() == LoginUpdate{login::Url{"https://auth.example/authorize", "http://127.0.0.1:9/l", std::nullopt}});
    const auto received = fake.received();
    CHECK(std::ranges::any_of(received, [](const auto& f) { return str(f, "type") == "login" && str(f, "providerId") == "anthropic"; }));
    CHECK(std::ranges::any_of(received, [](const auto& f) {
        return str(f, "type") == "extension_ui_response" && str(f, "id") == "u2" && str(f, "value") == "http://127.0.0.1:9/cb?code=1";
    }));
    block_on(exec, (*client)->shutdown(std::chrono::seconds(10)));
}

TEST_CASE("omp adapter: cancelling a login withdraws its pending input") {
    loa::runtime::Executor exec(3);
    auto dir = loa::runtime::TempDir::create().value();
    const std::string script = handshake_v2() +
        "< login lg\n"
        "> {\"type\":\"extension_ui_request\",\"id\":\"u1\",\"method\":\"open_url\",\"url\":\"https://auth.example/authorize\"}\n"
        "> {\"type\":\"extension_ui_request\",\"id\":\"u2\",\"method\":\"input\",\"title\":\"Paste code\"}\n"
        "< extension_ui_response\n";
    auto fake = FakeProcess::install(dir.path(), script).value();
    auto client = block_on(exec, OmpClient::spawn(launch_of(fake)));
    REQUIRE(client);
    OmpOAuthLogin strategy(*client, std::make_shared<Opener>(), mrpc::LoginProvider{"anthropic", "Anthropic", true, false});
    std::vector<LoginUpdate> seen;
    CHECK_FALSE(block_on_strand(exec, drive(strategy, "", seen, true)));
    REQUIRE_FALSE(seen.empty());
    CHECK(std::holds_alternative<login::Cancelled>(seen.back()));
    block_on(exec, (*client)->shutdown(std::chrono::seconds(10)));
    const auto received = fake.received();
    const auto cancelled = std::ranges::find_if(received, [](const auto& f) { return str(f, "type") == "extension_ui_response"; });
    REQUIRE(cancelled != received.end());
    CHECK(str(*cancelled, "id") == "u2");
    CHECK(json::dump(*cancelled->find("cancelled")) == "true");
}

TEST_CASE("omp adapter: an unavailable provider fails without starting a login") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto fake = FakeProcess::install(dir.path(), handshake_v2()).value();
    auto service = std::make_shared<OmpService>(launch_of(fake));
    OmpOAuthLogin strategy(service, std::make_shared<Opener>(), mrpc::LoginProvider{"x", "Paused", false, false});
    std::vector<LoginUpdate> seen;
    CHECK_FALSE(block_on_strand(exec, drive(strategy, "", seen)));
    REQUIRE(seen.size() == 1);
    CHECK(seen[0] == LoginUpdate{login::Failed{"omp offers no login for Paused now"}});
}

TEST_CASE("omp adapter: dialogs outside a login are dismissed and models decode") {
    loa::runtime::Executor exec(3);
    auto dir = loa::runtime::TempDir::create().value();
    const std::string script = handshake_v2() +
        "< get_available_models gm\n"
        "> {\"type\":\"extension_ui_request\",\"id\":\"d1\",\"method\":\"confirm\",\"title\":\"Trust?\"}\n"
        "< extension_ui_response\n"
        "> {\"type\":\"response\",\"id\":\"{{gm}}\",\"command\":\"get_available_models\",\"success\":true,\"data\":{\"models\":[{\"id\":\"m1\",\"name\":\"M One\",\"provider\":\"gw\",\"contextWindow\":null}]}}\n"
        "< set_model sm\n"
        "> {\"type\":\"response\",\"id\":\"{{sm}}\",\"command\":\"set_model\",\"success\":false,\"error\":\"Model not found: gw/nope\"}\n";
    auto fake = FakeProcess::install(dir.path(), script).value();
    OmpService service(launch_of(fake));
    auto listed = block_on(exec, service.get_available_models());
    REQUIRE(listed);
    REQUIRE(listed->size() == 1);
    CHECK((*listed)[0].selector() == "gw/m1");
    auto failed = block_on(exec, service.set_model("gw", "nope"));
    REQUIRE_FALSE(failed);
    CHECK(failed.error() == models::RpcError::command_failed("set_model", "Model not found: gw/nope"));
    block_on(exec, service.shutdown(std::chrono::seconds(10)));
    const auto received = fake.received();
    REQUIRE(received.size() >= 3);
    CHECK(str(received[2], "type") == "extension_ui_response");
    CHECK(str(received[2], "id") == "d1");
    CHECK(json::dump(*received[2].find("cancelled")) == "true");
}

TEST_CASE("omp adapter: logout lists stored credentials and removes one") {
    loa::runtime::Executor exec(3);
    auto dir = loa::runtime::TempDir::create().value();
    const std::string script = handshake_v2() +
        "< get_logout_accounts ga\n"
        "> {\"type\":\"response\",\"id\":\"{{ga}}\",\"command\":\"get_logout_accounts\",\"success\":true,\"data\":{\"accounts\":[{\"credentialId\":7,\"provider\":\"anthropic\",\"label\":\"me@example.com\",\"detail\":\"Claude Max\",\"type\":\"oauth\",\"active\":true},{\"credentialId\":9,\"provider\":\"anthropic\",\"label\":\"API key\",\"detail\":\"ends in 1234\",\"type\":\"api_key\",\"active\":false}]}}\n"
        "< logout lo\n"
        "> {\"type\":\"response\",\"id\":\"{{lo}}\",\"command\":\"logout\",\"success\":true,\"data\":{\"remainingSource\":\"ANTHROPIC_API_KEY\"}}\n"
        "< logout lx\n"
        "> {\"type\":\"response\",\"id\":\"{{lx}}\",\"command\":\"logout\",\"success\":true,\"data\":{}}\n"
        "< logout gone\n"
        "> {\"type\":\"response\",\"id\":\"{{gone}}\",\"command\":\"logout\",\"success\":false,\"error\":\"Credential 7 is not stored for anthropic\"}\n";
    auto fake = FakeProcess::install(dir.path(), script).value();
    OmpService service(launch_of(fake));
    auto accounts = block_on(exec, service.get_logout_accounts("anthropic"));
    REQUIRE(accounts);
    CHECK(*accounts == std::vector<mrpc::LogoutAccount>{{7, "anthropic", "me@example.com", "Claude Max", "oauth", true},
                                                        {9, "anthropic", "API key", "ends in 1234", "api_key", false}});
    auto first = block_on(exec, service.logout("anthropic", 7));
    REQUIRE(first);
    CHECK(*first == std::optional<std::string>("ANTHROPIC_API_KEY"));
    auto second = block_on(exec, service.logout("anthropic", 9));
    REQUIRE(second);
    CHECK_FALSE(second->has_value());
    auto gone = block_on(exec, service.logout("anthropic", 7));
    REQUIRE_FALSE(gone);
    CHECK(gone.error() == models::RpcError::command_failed("logout", "Credential 7 is not stored for anthropic"));
    block_on(exec, service.shutdown(std::chrono::seconds(10)));
    std::vector<std::string> sent;
    for (auto frame : fake.received()) {
        if (str(frame, "type") == "negotiate_protocol") continue;
        auto& object = *frame.as_object();
        std::erase_if(object, [](const auto& entry) { return entry.first == "id"; });
        sent.push_back(json::dump(frame));
    }
    CHECK(sent == std::vector<std::string>{
                      R"({"type":"get_logout_accounts","providerId":"anthropic"})",
                      R"({"type":"logout","providerId":"anthropic","credentialId":7})",
                      R"({"type":"logout","providerId":"anthropic","credentialId":9})",
                      R"({"type":"logout","providerId":"anthropic","credentialId":7})",
                  });
}

TEST_CASE("omp adapter: account edits the default role without losing other candidates") {
    auto dir = loa::runtime::TempDir::create().value();
    const auto program = dir.path() / "omp";
    REQUIRE(loa::runtime::exec::write_executable(
        program, "#!/bin/sh\nif [ \"$1 $2\" = 'config path' ]; then printf '%s\\n' \"$TEST_AGENT_DIR\"; exit 0; fi\nexit 64\n"));
    const auto config = dir.path() / "config.yml";
    std::ofstream(config) << "# keep\nmodelRoles:\n  default: old/model:low,other/model\n";
    OmpLaunch launch;
    launch.executable = program;
    launch.env = {{"TEST_AGENT_DIR", dir.path().string()}};
    OmpAccount account(std::make_shared<OmpService>(launch), std::make_shared<Opener>(), launch);
    auto before = account.read_model();
    REQUIRE(before);
    CHECK(*before == ModelSettings{"old/model", "low"});
    const ModelSettings after{"new/model", "high"};
    REQUIRE(account.write_model(after));
    auto reread = account.read_model();
    REQUIRE(reread);
    CHECK(*reread == after);
    const auto text = read_file(config);
    CHECK(text.find("# keep") != std::string::npos);
    CHECK(text.find("new/model:high,other/model") != std::string::npos);
}

TEST_CASE("omp adapter: the service starts a new omp after the old one exited") {
    loa::runtime::Executor exec(3);
    auto dir = loa::runtime::TempDir::create().value();
    auto fake = FakeProcess::install(dir.path(), handshake_v2() + "exit 0\n").value();
    OmpService service(launch_of(fake));
    auto first = block_on(exec, service.client());
    REQUIRE(first);
    for (int i = 0; i < 200 && !(*first)->has_exited(); ++i) std::this_thread::sleep_for(std::chrono::milliseconds(10));
    CHECK((*first)->has_exited());
    auto second = block_on(exec, service.client());
    REQUIRE(second);
    CHECK(first->get() != second->get());
    block_on(exec, service.shutdown(std::chrono::seconds(10)));
}

namespace {

class Runner final : public loa::harness::account_support::ProcessRunner {
  public:
    mutable std::mutex mutex;
    mutable std::vector<CommandSpec> calls;
    std::optional<int> code = 0;
    std::string err;
    Awaitable<models::Result<loa::harness::account_support::ProcessOutput>> output(CommandSpec spec) const override {
        std::string text;
        if (spec.current_dir) text = read_file(*spec.current_dir / "config.yml");
        {
            std::scoped_lock lock(mutex);
            calls.push_back(spec);
        }
        std::string out = text.find("modelRoles:") != std::string::npos ? R"({"modelRoles":{"value":{"smol":"openai/gpt"}}})" : "[]";
        co_return loa::harness::account_support::ProcessOutput{code, std::move(out), err};
    }
    Awaitable<models::Result<std::unique_ptr<loa::harness::account_support::RunningProcess>>> spawn(CommandSpec) const override {
        co_return std::unexpected(models::Error::parse("spawn", "unused"));
    }
};

} // namespace

TEST_CASE("omp adapter: settings validator checks written roles in an isolated agent directory") {
    loa::runtime::Executor exec(2);
    auto runner = std::make_shared<Runner>();
    OmpSettingsValidator validator("omp", runner);
    auto smol = json::parse(R"({"smol":"openai/gpt"})").value();
    auto ok = block_on(exec, validator.check_config("modelRoles:\n  smol: openai/gpt\n", {{"modelRoles", smol}}));
    REQUIRE_MESSAGE(ok, (ok ? std::string() : ok.error().to_string()));
    REQUIRE(runner->calls.size() == 1);
    CHECK(runner->calls[0].args == std::vector<std::string>{"config", "list", "--scope", "global", "--json"});
    REQUIRE(runner->calls[0].current_dir.has_value());
    const auto& env = runner->calls[0].env;
    CHECK(std::ranges::any_of(env, [&](const auto& kv) { return kv.first == "PI_CODING_AGENT_DIR" && kv.second == runner->calls[0].current_dir->string(); }));
    CHECK(std::ranges::any_of(env, [](const auto& kv) { return kv.first == "OMP_PROFILE" && kv.second == "default"; }));
    CHECK(std::ranges::any_of(env, [](const auto& kv) { return kv.first == "PI_CONFIG_FILES" && kv.second.empty(); }));
    CHECK_FALSE(fs::exists(*runner->calls[0].current_dir));

    auto other = json::parse(R"({"smol":"other/model"})").value();
    auto rejected = block_on(exec, validator.check_config("modelRoles:\n  smol: openai/gpt\n", {{"modelRoles", other}}));
    REQUIRE_FALSE(rejected);
    CHECK(rejected.error().kind == models::Error::Kind::Rejected);
    CHECK(rejected.error().message == "omp did not accept setting modelRoles");

    auto listed = block_on(exec, validator.check_models("providers: {}\n"));
    CHECK(listed);
    CHECK(runner->calls.back().args == std::vector<std::string>{"models", "ls", "--json", "--no-extensions"});

    runner->err = "  bad models.yml entry\n";
    auto models_rejected = block_on(exec, validator.check_models("providers: {}\n"));
    REQUIRE_FALSE(models_rejected);
    CHECK(models_rejected.error().kind == models::Error::Kind::Rejected);
    CHECK(models_rejected.error().message == "bad models.yml entry");
    runner->code = 2;
    runner->err = "boom\n";
    auto failed = block_on(exec, validator.check_config("x: 1\n", {}));
    REQUIRE_FALSE(failed);
    CHECK(failed.error().message == "boom");
}
