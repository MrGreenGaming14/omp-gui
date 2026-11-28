// Port of crates/loa-models/src/service_tests.rs and the unit tests of service.rs: the
// service against a fake omp RPC, an offline omp account and a temp agent dir.

#include <doctest/doctest.h>

#include <atomic>
#include <fstream>
#include <sstream>

#include <asio/as_tuple.hpp>
#include <asio/co_spawn.hpp>
#include <asio/io_context.hpp>
#include <asio/use_awaitable.hpp>
#include <asio/use_future.hpp>

#include "loa/contracts/login_channels.hpp"
#include "loa/models/login_updates.hpp"
#include "loa/models/service.hpp"
#include "loa/models/testing.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::models;
using namespace loa::models::testing;
namespace rpc = loa::settings::models::rpc;
namespace contracts = loa::contracts;
using loa::settings::models::settings::RoleChain;

namespace {

template <typename T>
T run(contracts::Awaitable<T> task) {
    asio::io_context io;
    auto future = asio::co_spawn(io, std::move(task), asio::use_future);
    io.run();
    return future.get();
}

std::string read(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::stringstream s;
    s << in.rdbuf();
    return s.str();
}

void write(const std::filesystem::path& path, std::string_view text) {
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    out << text;
}

std::shared_ptr<FakeOmp> omp_rpc() {
    auto fake = std::make_shared<FakeOmp>();
    fake->providers = {{"anthropic", "Anthropic (Claude Pro/Max)", true, true},
                       {"openai-codex", "ChatGPT Plus/Pro (Codex Subscription)", true, false}};
    const auto model = [](std::string provider, std::string id) {
        rpc::ModelInfo m;
        m.name = id;
        for (auto& c : m.name) c = static_cast<char>(std::toupper(static_cast<unsigned char>(c)));
        m.id = std::move(id);
        m.provider = std::move(provider);
        return m;
    };
    fake->models = {model("anthropic", "claude-x"), model("openai-codex", "gpt-y")};
    const auto credential = [](std::int64_t id, std::string provider, std::string label, bool active) {
        return rpc::LogoutAccount{id, std::move(provider), std::move(label), "OAuth", "oauth", active};
    };
    fake->set_credentials({credential(7, "anthropic", "me@example.com", true), credential(8, "openai-codex", "me@example.com", true),
                           credential(9, "anthropic", "work@example.com", false)});
    return fake;
}

struct Fixture {
    loa::runtime::TempDir dir;
    Backend backend;
};

Fixture fixture(std::string_view config, std::vector<std::shared_ptr<contracts::HarnessAccount>> harnesses = {}) {
    auto dir = loa::runtime::TempDir::create().value();
    write(dir.path() / "config.yml", config);
    write(dir.path() / "models.yml", "");
    Backend backend(Parts{omp_rpc(), std::make_shared<OfflineOmpAccount>(), OmpFiles{dir.path()},
                          std::make_shared<RejectingValidator>(), std::make_shared<NoBrowser>(), std::move(harnesses),
                          std::make_shared<NoEnv>()});
    return Fixture{std::move(dir), std::move(backend)};
}

// A login that ends on Cancel, or when its controls close.
class WaitLogin final : public contracts::LoginStrategy {
  public:
    contracts::Awaitable<void> run(std::shared_ptr<contracts::LoginChannels> channels) override {
        for (;;) {
            auto [ec, control] = co_await channels->control.async_receive(asio::as_tuple(asio::use_awaitable));
            if (ec) co_return;
            if (std::holds_alternative<contracts::CancelLogin>(control)) {
                co_await channels->updates.async_send(asio::error_code{}, contracts::login::Cancelled{},
                                                      asio::as_tuple(asio::use_awaitable));
                co_return;
            }
        }
    }
};

constexpr std::string_view FAKE_EFFORTS[] = {"low", "high"};

class FakeAccount final : public contracts::HarnessAccount {
  public:
    mutable contracts::ModelSettings stored;
    mutable int writes = 0;

    std::string_view id() const override { return "fake"; }
    std::string_view name() const override { return "Fake"; }
    contracts::Awaitable<contracts::SignInState> sign_in_state() const override {
        co_return contracts::SignedIn{{contracts::SignInMethod::Kind::Subscription, {}}};
    }
    contracts::Result<std::unique_ptr<contracts::LoginStrategy>> login(contracts::LoginKind) const override {
        return std::make_unique<WaitLogin>();
    }
    std::optional<contracts::CommandSpec> logout_command() const override { return std::nullopt; }
    std::span<const std::string_view> efforts() const override { return FAKE_EFFORTS; }
    std::optional<std::filesystem::path> config_path() const override { return std::nullopt; }
    contracts::Result<contracts::ModelSettings> read_model() const override { return stored; }
    contracts::Result<> write_model(const contracts::ModelSettings& m) const override {
        stored = m;
        ++writes;
        return {};
    }
};

// Finds `dir` on every call after the first `failures` calls, counting calls.
class Resolver final : public loa::settings::models::files::OmpFilesResolver {
  public:
    Resolver(std::filesystem::path dir, int failures) : dir_(std::move(dir)), failures_(failures) {}
    std::atomic<int> calls{0};
    contracts::Awaitable<loa::settings::models::Result<OmpFiles>> resolve() const override {
        if (const_cast<std::atomic<int>&>(calls).fetch_add(1) < failures_) {
            co_return std::unexpected(loa::settings::models::Error::process("omp", "`config path` failed: boom"));
        }
        co_return OmpFiles{dir_};
    }

  private:
    std::filesystem::path dir_;
    int failures_;
};

Backend backend_with(FilesSource files, std::vector<std::shared_ptr<contracts::HarnessAccount>> harnesses) {
    return Backend(Parts{std::make_shared<FakeOmp>(), std::make_shared<OfflineOmpAccount>(), std::move(files),
                         std::make_shared<RejectingValidator>(), std::make_shared<NoBrowser>(), std::move(harnesses),
                         std::make_shared<NoEnv>()});
}

} // namespace

TEST_CASE("the_catalog_resolves_roles_and_assigns_them_in_omp_config") {
    auto f = fixture("modelRoles:\n  smol: anthropic/claude-x:low # fast\n  alias: '@smol'\n  missing: absent/model\n");
    const auto catalog = run(f.backend.catalog()).value();
    REQUIRE(catalog.models.size() == 2);
    CHECK(catalog.models[0].provider_name == "Anthropic (Claude Pro/Max)");
    const ModelRef claude{"anthropic", "claude-x"};
    // Aliases resolve; unavailable models drop out.
    CHECK(catalog.roles == std::vector<RoleAssignment>{{"smol", claude}, {"alias", claude}});
    REQUIRE(run(f.backend.assign_role("smol", ModelRef{"openai-codex", "gpt-y"})));
    const auto text = read(f.dir.path() / "config.yml");
    CHECK_MESSAGE(text.starts_with("modelRoles:\n  smol: openai-codex/gpt-y:low # fast\n"), text);
}

TEST_CASE("settings_save_through_validation_and_refuse_stale_files") {
    auto f = fixture("# keep me\nmodelRoles:\n  default: anthropic/claude-x\n");
    const auto config = f.dir.path() / "config.yml";
    const auto load = run(f.backend.load_settings()).value();
    CHECK(load.known_models == std::vector<std::string>{"anthropic/claude-x", "openai-codex/gpt-y"});
    auto settings = load.settings;
    settings.roles[0].chain.push_back("openai-codex/gpt-y");
    const auto saved = run(f.backend.save_settings(settings)).value();
    CHECK(saved.roles == std::vector<RoleChain>{{"default", {"anthropic/claude-x", "openai-codex/gpt-y"}}});
    const auto text = read(config);
    CHECK_MESSAGE(text.starts_with("# keep me\n"), text);
    CHECK_MESSAGE(text.find("fallbackChains:\n    default:\n      - openai-codex/gpt-y\n") != std::string::npos, text);

    // omp's rejection comes back and nothing is written.
    auto rejected = saved;
    rejected.roles.push_back({"reject-me", {"anthropic/claude-x"}});
    const auto rejection = run(f.backend.save_settings(rejected));
    REQUIRE_FALSE(rejection);
    CHECK(rejection.error()[0].location == "config.yml");
    CHECK(read(config) == text);

    // A change made outside the app since the load is not overwritten.
    write(config, "modelRoles: {}\n");
    auto stale = saved;
    stale.roles[0].chain.pop_back();
    const auto errors = run(f.backend.save_settings(stale));
    REQUIRE_FALSE(errors);
    CHECK_MESSAGE(errors.error()[0].message.find("changed on disk") != std::string::npos, errors.error()[0].message);
    CHECK(read(config) == "modelRoles: {}\n");
}

TEST_CASE("signing_out_of_omp_removes_one_stored_credential_at_a_time") {
    auto f = fixture("");
    const ProviderKey anthropic{std::string(OMP), "anthropic"};
    const auto signed_in = [&] {
        for (auto& row : run(f.backend.providers())) {
            if (row.key == anthropic) return row.auth;
        }
        FAIL("no anthropic row");
        return Auth{};
    };
    CHECK(signed_in() == Auth(auth::Login{true, std::nullopt}));
    const auto credentials = run(f.backend.credentials(anthropic)).value();
    // Only the provider's own, as omp lists them.
    REQUIRE(credentials.size() == 2);
    CHECK((credentials[0].id == 7 && credentials[0].label == "me@example.com" && credentials[0].active));
    CHECK((credentials[1].id == 9 && credentials[1].label == "work@example.com" && !credentials[1].active));

    CHECK(run(f.backend.sign_out(anthropic, 7)) == std::optional<std::string>{});
    // Another credential still signs in.
    CHECK(signed_in() == Auth(auth::Login{true, std::nullopt}));
    const auto gone = run(f.backend.sign_out(anthropic, 7));
    REQUIRE_FALSE(gone);
    CHECK_MESSAGE(gone.error().find("Credential 7 is not stored for anthropic") != std::string::npos, gone.error());
    // omp removes credentials by id only.
    CHECK(run(f.backend.sign_out(anthropic, std::nullopt)).error() == "anthropic has no such stored sign in");

    CHECK(run(f.backend.sign_out(anthropic, 9)) == std::optional<std::string>{});
    CHECK(signed_in() == Auth(auth::Login{false, std::nullopt}));
    CHECK(run(f.backend.credentials(anthropic)).value().empty());
    CHECK(run(f.backend.credentials(ProviderKey{"codex", "codex"})).error() == "unknown harness codex");
}

TEST_CASE("fixed_files_are_used_as_given") {
    auto dir = loa::runtime::TempDir::create().value();
    auto backend = backend_with(OmpFiles{dir.path()}, {});
    CHECK(run(backend.files()).value() == OmpFiles{dir.path()});
    CHECK(run(backend.files()).value().config() == dir.path() / "config.yml");
}

TEST_CASE("resolved_files_are_kept_and_failures_returned_as_they_are") {
    auto dir = loa::runtime::TempDir::create().value();
    auto resolver = std::make_shared<Resolver>(dir.path(), 1);
    auto backend = backend_with(resolver, {});
    CHECK(run(backend.files()).error() == "cannot run omp: `config path` failed: boom");
    CHECK(run(backend.files()).value() == OmpFiles{dir.path()});
    CHECK(run(backend.files()).value() == OmpFiles{dir.path()});
    // A failure is asked again, a success is kept.
    CHECK(resolver->calls.load() == 2);
}

TEST_CASE("a_replaced_login_finishing_keeps_the_new_logins_controls") {
    auto dir = loa::runtime::TempDir::create().value();
    auto backend = backend_with(OmpFiles{dir.path()}, {std::make_shared<FakeAccount>()});
    const ProviderKey key{"fake", "fake"};
    asio::io_context io;
    auto done = asio::co_spawn(
        io,
        [&]() -> contracts::Awaitable<std::pair<bool, std::optional<LoginUpdate>>> {
            auto first = backend.login(io.get_executor(), key);
            auto second = backend.login(io.get_executor(), key);
            // Registering the second login closed the first one's controls.
            auto [first_ec, first_update] = co_await first->channel.async_receive(asio::as_tuple(asio::use_awaitable));
            backend.cancel(key);
            auto [second_ec, second_update] = co_await second->channel.async_receive(asio::as_tuple(asio::use_awaitable));
            co_return std::pair{static_cast<bool>(first_ec), second_ec ? std::nullopt : std::optional(second_update)};
        },
        asio::use_future);
    io.run();
    const auto [first_closed, second] = done.get();
    CHECK(first_closed);
    CHECK(second == std::optional<LoginUpdate>(contracts::login::Cancelled{}));
}

TEST_CASE("harness model edits are checked with the Rust error text and written field by field") {
    auto account = std::make_shared<FakeAccount>();
    account->stored = {std::string("m1"), std::string("low")};
    auto f = fixture("", {account});
    const auto load = run(f.backend.load_settings()).value();
    REQUIRE(load.settings.harnesses.size() == 1);
    CHECK(load.settings.harnesses[0] ==
          HarnessModel{"fake", "Fake", "", "m1", "low", {"low", "high"}});

    const auto save_harness = [&](std::string model, std::string effort) {
        auto settings = load.settings;
        settings.harnesses[0].model = std::move(model);
        settings.harnesses[0].effort = std::move(effort);
        return run(f.backend.save_settings(settings));
    };
    const auto spaced = save_harness("my model", "low");
    REQUIRE_FALSE(spaced);
    CHECK(spaced.error() == std::vector<ValidationError>{{"Fake", "model `my model` must have no spaces"}});
    const auto control = save_harness("m\x01", "low");
    REQUIRE_FALSE(control);
    CHECK(control.error()[0].message == "model `m\x01` must have no spaces");
    const auto nbsp = save_harness("m\xC2\xA0x", "low");
    REQUIRE_FALSE(nbsp);
    const auto effort = save_harness("m1", "max");
    REQUIRE_FALSE(effort);
    CHECK(effort.error() == std::vector<ValidationError>{{"Fake", "effort `max` is not one of low, high"}});
    CHECK(account->writes == 0);

    // Only changed fields are written; an empty value clears.
    account->stored.model = "outside";
    const auto saved = save_harness("m1", "");
    REQUIRE(saved);
    CHECK(account->stored == contracts::ModelSettings{std::string("outside"), std::nullopt});
    CHECK(account->writes == 1);
}

TEST_CASE("golden models.yml and config.yml are untouched by a no-op save and edited by line") {
    const std::filesystem::path golden = LOA_GOLDEN_DIR "/settings";
    auto dir = loa::runtime::TempDir::create().value();
    const auto config_text = read(golden / "config.yml");
    const auto models_text = read(golden / "models.yml");
    write(dir.path() / "config.yml", config_text);
    write(dir.path() / "models.yml", models_text);
    auto backend = backend_with(OmpFiles{dir.path()}, {});

    const auto load = run(backend.load_settings()).value();
    REQUIRE(run(backend.save_settings(load.settings)));
    CHECK(read(dir.path() / "config.yml") == config_text);
    CHECK(read(dir.path() / "models.yml") == models_text);

    REQUIRE(run(backend.assign_role("plan", ModelRef{"anthropic", "claude-x"})));
    auto expected = config_text;
    const std::string before = "  plan: openai/gpt-y:low\n";
    expected.replace(expected.find(before), before.size(), "  plan: anthropic/claude-x:low\n");
    CHECK(read(dir.path() / "config.yml") == expected);
    CHECK(read(dir.path() / "models.yml") == models_text);
}
