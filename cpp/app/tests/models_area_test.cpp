// The models area (port of `loa-app/src/areas/models/{tests,session}.rs`): the composer's model
// and effort pickers, role rows of the picker with and without a running session, signing out,
// the session's recorded role and the session config a pending pick contributes.

#include <filesystem>
#include <fstream>

#include <doctest/doctest.h>

#include "areas/models/models.hpp"
#include "areas/models/session.hpp"
#include "loa/app/view/settings/models.hpp"
#include "loa/core/schema.hpp"
#include "loa/core/sink.hpp"
#include "loa/models/testing.hpp"
#include "support/test_app.hpp"

using namespace loa::app;
using loa::app::testing::FakeHarness;
using loa::app::testing::TestApp;
namespace m = loa::app::areas::models;
namespace attr = loa::core::attr;
namespace fs = std::filesystem;
namespace picker = loa::ui::settings::models::picker;
namespace signin = loa::ui::settings::models::signin;
namespace vm = loa::view::settings::models;
using loa::core::Draft;
using loa::core::ElementKind;
using loa::core::NodeId;

namespace {

void write_file(const fs::path& path, std::string_view text) {
    fs::create_directories(path.parent_path());
    std::ofstream(path, std::ios::binary) << text;
}

std::optional<std::string> root_attr(const App& app, std::string_view name) {
    const auto* active = app.ctx.runtime.sessions().active();
    const auto* root = active ? active->dom().root() : nullptr;
    auto value = root ? root->str_attr(name) : std::nullopt;
    return value ? std::optional<std::string>(*value) : std::nullopt;
}

// Puts the picker up over a catalog with only role `DEEP`, on `provider/deep`.
void deep_role_picker(TestApp& test) {
    m::Catalog catalog{{}, {m::RoleAssignment{"DEEP", m::ModelRef{"provider", "deep"}}}};
    auto [state, focus] = picker::State::open(view::settings::catalog_to_view(catalog), vm::picker::target::Session{}, std::nullopt);
    (void)focus;
    test.area<m::State>().set_picker("omp", m::OpenPicker{std::move(state), {}});
}

// Fills `sink` with a `<session>` root with the given model attributes.
void session_sink(loa::core::MemorySink& sink, std::optional<std::string> model, std::optional<std::string> role) {
    loa::core::Attrs attrs{{std::string(attr::ID), "s-1"}, {std::string(attr::HARNESS), "omp"}, {std::string(attr::NODE), "node"},
                           {std::string(attr::CREATED), std::int64_t{1}}};
    if (model) attrs.insert_or_assign(std::string(attr::MODEL), *model);
    if (role) attrs.insert_or_assign(std::string(attr::MODEL_ROLE), *role);
    REQUIRE(sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, std::move(attrs))));
}

} // namespace

TEST_CASE("models: role drafts record only changes") {
    using O = std::optional<std::string>;
    struct Case {
        O recorded, picked, after;
        std::size_t writes;
    };
    const Case cases[] = {{std::nullopt, "default", "default", 1},
                          {"default", std::nullopt, std::nullopt, 1},
                          {"default", "plan", "plan", 1},
                          {"plan", "plan", "plan", 0},
                          {std::nullopt, std::nullopt, std::nullopt, 0}};
    for (const auto& c : cases) {
        loa::core::MemorySink sink;
        session_sink(sink, "anthropic/claude", c.recorded);
        auto picked = c.picked ? std::optional<std::string_view>(*c.picked) : std::nullopt;
        auto draft = m::role_draft(sink.dom(), picked);
        CHECK(static_cast<std::size_t>(draft.has_value()) == c.writes);
        if (draft) REQUIRE(sink.commit(std::move(*draft)));
        CHECK(m::session_model(sink.dom())->second == vm::SessionModel{"anthropic/claude", c.after});
    }
    CHECK_MESSAGE(!m::role_draft(loa::core::Dom{}, "default"), "no session, nothing to record");
    CHECK_FALSE(m::session_model(loa::core::Dom{}).has_value());
}

TEST_CASE("models: failures from adapter errors") {
    using loa::contracts::AdapterError;
    auto unsupported = m::Failure::from(AdapterError(AdapterError::Unsupported{"fake", "model switching"}));
    CHECK(unsupported.kind == m::Failure::Kind::Unsupported);
    CHECK(unsupported.text == AdapterError(AdapterError::Unsupported{"fake", "model switching"}).message());
    auto other = m::Failure::from(AdapterError(AdapterError::Protocol{"bad frame"}));
    CHECK(other.kind == m::Failure::Kind::Error);
    CHECK(other.text == AdapterError(AdapterError::Protocol{"bad frame"}).message());
}

TEST_CASE("models: the composer lists the harness models and switches model and effort") {
    const loa::harness::testing::ScriptedModel models[] = {{"a/one", {"low", "high"}}, {"b/two", {}}};
    TestApp test(FakeHarness("fake").with_models(models).with_effort());
    test.session("hello");
    test.until("the models are listed", [](const App& app) { return app.ctx.composer.models().first.size() == 2; });
    test.command("composer.model.1");
    test.until("the model switched", [](const App& app) { return root_attr(app, attr::MODEL) == "b/two"; });
    test.prompt("/model gone/x");
    test.until("the failure is shown", [](const App& app) {
        return app.ctx.notice() && app.ctx.notice()->find("gone/x") != std::string::npos;
    });
    test.command("models.settings");
    CHECK(test.app->ctx.screen() == std::optional<std::string_view>("models"));
}

TEST_CASE("models: picking a role row switches role and thinking") {
    auto caps = loa::contracts::Capabilities::builder().model_switch(loa::contracts::ModelSwitch::PerTurn).build();
    const loa::harness::testing::ScriptedModel models[] = {{"provider/deep", {"high"}}};
    TestApp test(FakeHarness("omp").with_capabilities(caps).with_models(models).with_effort());
    write_file(test.dir.path() / "omp" / "config.yml", "modelRoles:\n  DEEP: provider/deep:high\n");
    test.session("hello");
    deep_role_picker(test);
    test.send(m::State::message(m::msg::Picker{picker::Message::local(picker::local::Pick{0})}));
    test.until("role switches", [](const App& app) { return root_attr(app, attr::MODEL_ROLE) == "DEEP"; });
    test.prompt("hello again");
    test.until("turn ends", [](const App& app) { return !app.ctx.composer.is_busy(); });
    const auto seen = test.harness.seen();
    CHECK(seen.models == std::vector<std::string>{"provider/deep"});
    CHECK(seen.efforts == std::vector<std::string>{"high"});
    CHECK(std::ranges::find(seen.prompts, "The active model role is now `DEEP`.\n\nhello again") != seen.prompts.end());
}

TEST_CASE("models: picking a role row without a session configures the next one") {
    const loa::harness::testing::ScriptedModel models[] = {{"provider/deep", {"high"}}};
    TestApp test(FakeHarness("omp").with_models(models).with_effort());
    const auto dir = test.dir.path() / "omp";
    write_file(dir / "config.yml", "modelRoles:\n  DEEP: provider/deep:high\n");
    // A RULES.md the workflows screen refuses doesn't stop picking a role: only config.yml is read.
    write_file(dir / "RULES.md", "<!-- omp-gui:workflow BROKEN -->\n");
    deep_role_picker(test);
    test.send(m::State::message(m::msg::Picker{picker::Message::local(picker::local::Pick{0})}));
    CHECK(test.area<m::State>().pending().contains(std::nullopt));
    test.session("hello");
    const auto seen = test.harness.seen();
    REQUIRE_FALSE(seen.configs.empty());
    const auto& config = seen.configs.back();
    CHECK(config.model == std::optional<std::string>("provider/deep"));
    CHECK(config.model_role == std::optional<std::string>("DEEP"));
    CHECK(config.effort == std::optional<std::string>("high"));
    CHECK(config.instructions == std::optional<std::string>("The active model role is `DEEP`. Follow the RULES.md section for that role."));
    CHECK_MESSAGE(test.area<m::State>().pending().empty(), "the pick went to that session");
}

TEST_CASE("models: signing out removes the picked sign in and reads the rows again") {
    namespace rpc = loa::settings::models::rpc;
    TestApp test;
    auto omp = std::make_shared<loa::models::testing::FakeOmp>();
    omp->providers = {rpc::LoginProvider{"anthropic", "Anthropic", true, true}};
    omp->set_credentials({rpc::LogoutAccount{7, "anthropic", "me@example.com", "Claude Max", "oauth", true}});
    omp->remaining_source = "ANTHROPIC_API_KEY";
    test.area<m::State>().backend = loa::models::Backend(TestApp::offline_models(omp, test.dir.path()));
    test.command("models.sign-in");
    auto rows = [&]() -> const signin::State& {
        const auto* state = std::get_if<signin::State>(&test.area<m::State>().signin());
        REQUIRE_MESSAGE(state, "the sign in rows were read");
        return *state;
    };
    REQUIRE_FALSE(rows().providers().empty());
    const auto key = rows().providers()[0].key;
    const vm::Auth signed_in{vm::auth::Login{true, std::nullopt}};
    const vm::Auth signed_out{vm::auth::Login{false, std::nullopt}};
    CHECK(rows().providers()[0].auth == signed_in);
    auto local = [&](signin::Local message) { test.send(m::State::message(m::msg::SignIn{signin::Message::local(std::move(message))})); };
    local(signin::local::SignOut{key});
    // One stored sign in, one Remove: picking it removes credential 7 in omp.
    local(signin::local::Remove{key, 7});
    CHECK_MESSAGE(rows().providers()[0].auth == signed_out, "the rows are read again");
    // The outcome stays on the row through the refresh until dismissed.
    CHECK(rows().sign_out(key) != nullptr);
    local(signin::local::Dismiss{key});
    CHECK(rows().sign_out(key) == nullptr);
}

TEST_CASE("models: a pending pick goes only to a session on its harness") {
    TestApp test;
    auto& state = test.area<m::State>();
    test.send(m::State::message(m::msg::Model{{"a/one", "one", "a", std::nullopt}}));
    CHECK(test.notice() == "The next session starts on a/one");
    loa::contracts::SessionConfig config;
    state.contribute(config, Target{"other", nullptr, false}, test.app->ctx);
    CHECK_FALSE(config.model.has_value());
    state.contribute(config, Target{"fake", nullptr, false}, test.app->ctx);
    CHECK(config.model == std::optional<std::string>("a/one"));
    CHECK_FALSE(config.model_role.has_value());
    CHECK(state.pending().empty());
}
