// Port of the inline tests of `crates/loa-ui-settings/src/models/settings.rs`, plus state
// tests of `OVERRIDE_FIELDS` order and Save/set_errors.

#include <doctest/doctest.h>

#include <algorithm>
#include <functional>
#include <string>
#include <vector>

#include "loa/ui/settings/models/view.hpp"
#include "support.hpp"

using namespace loa::ui::settings::models;
using namespace loa::ui::settings::models::settings;
namespace vm = loa::view::settings::models;
namespace vi = vm::settings::intent;
namespace l = loa::ui::settings::models::settings::local;

namespace {

ModelSettings model_settings() {
    auto provider = [](std::string id, std::string name, bool enabled) { return vm::ProviderToggle{std::move(id), std::move(name), enabled}; };
    ModelSettings s;
    s.roles = {{"default", {"anthropic/claude-sonnet-4-5:high", "openai/gpt-5", "google/gemini-2.5-pro"}}};
    s.models = {{"openai/gpt-5", true}};
    s.providers = {provider("anthropic", "Anthropic", true), provider("openai", "OpenAI", true), provider("google", "Google", false)};
    s.overrides = {{"openai/gpt-5", {{"contextWindow", FieldValue{std::int64_t{400'000}}}}}};
    s.harnesses = {{"codex", "Codex", "~/.codex/config.toml", "", "", {"low", "high"}}};
    return s;
}

CustomProvider local_provider() {
    return {"local-llm", "http://localhost:8080/v1", "openai-completions", AuthMode::ApiKey, "LOCAL_LLM_KEY", {"qwen3", "llama-4"}};
}

State state() { return State(model_settings(), {"openai/gpt-5", "anthropic/claude-haiku-4-5"}); }

std::optional<Intent> run(State& s, std::vector<Local> messages) {
    std::optional<Intent> last;
    for (auto& m : messages) last = s.update(Message::local(std::move(m)));
    return last;
}

std::optional<Intent> saved(const std::function<void(ModelSettings&)>& f) {
    auto s = model_settings();
    f(s);
    return vi::Save{std::move(s)};
}

} // namespace

TEST_CASE("OVERRIDE_FIELDS keys and order are omp's config keys") {
    const std::vector<std::string_view> want{"name", "contextWindow", "maxContextWindow", "maxTokens", "reasoning", "contextPromotionTarget", "compactionModel", "preferWebsockets"};
    std::vector<std::string_view> got;
    for (const auto& f : override_form()) got.push_back(f.key);
    CHECK(got == want);
}

TEST_CASE("edits save as settings") {
    using F = std::function<void(ModelSettings&)>;
    auto edit = [](std::size_t i, std::string key, std::string v) { return l::ProviderEdit{i, std::move(key), std::move(v)}; };
    const std::vector<std::pair<std::vector<Local>, std::optional<Intent>>> cases{
        {{l::MoveChain{0, 2, 0}, l::Save{}}, saved(F([](auto& s) { std::ranges::rotate(s.roles[0].chain, s.roles[0].chain.end() - 1); }))},
        {{l::AddChain{0, "anthropic/claude-haiku-4-5"}, l::ChainInput{0, " local/qwen3 "}, l::AddChain{0, std::nullopt}, l::AddChain{0, "openai/gpt-5"}, l::RemoveChain{0, 1}, l::Save{}},
         saved(F([](auto& s) { s.roles[0].chain = {"anthropic/claude-sonnet-4-5:high", "google/gemini-2.5-pro", "anthropic/claude-haiku-4-5", "local/qwen3"}; }))},
        {{l::MoveProvider{2, 1}, l::MoveProvider{0, 1}, l::MoveProvider{5, 0}, l::Save{}},
         saved(F([](auto& s) { s.providers = {s.providers[2], s.providers[0], s.providers[1]}; }))},
        {{l::OverrideEdit{0, "contextWindow", "12x"}, l::Save{}}, std::nullopt},
        {{l::OverrideEdit{0, "contextWindow", "128000"}, l::OverrideEdit{0, "reasoning", "true"}, l::Save{}},
         saved(F([](auto& s) { s.overrides[0].fields = {{"contextWindow", FieldValue{std::int64_t{128'000}}}, {"reasoning", FieldValue{true}}}; }))},
        {{l::AddOverride{"anthropic/claude-haiku-4-5"}, l::AddOverride{"openai/gpt-5"}, l::OverrideEdit{1, "name", " Haiku "}, l::OverrideEdit{1, "contextWindow", "1"}, l::Save{}},
         saved(F([](auto& s) { s.overrides.push_back({"anthropic/claude-haiku-4-5", {{"name", FieldValue{std::string("Haiku")}}, {"contextWindow", FieldValue{std::int64_t{1}}}}}); }))},
        {{l::AddOverride{"a/b"}, l::OverrideEdit{1, "reasoning", "maybe"}, l::Save{}}, std::nullopt},
        {{l::NewRole{"plan"}, l::AddRole{}, l::RenameRole{1, "default"}, l::Save{}}, std::nullopt},
        {{l::AddProvider{}, l::Save{}}, std::nullopt},
        {{l::AddProvider{}, edit(0, "id", "local-llm"), edit(0, "base_url", "http://localhost:8080/v1"), edit(0, "api", "openai-completions"), edit(0, "api_key_env", "LOCAL_LLM_KEY"),
          edit(0, "models", " qwen3 ,, llama-4 "), l::Save{}},
         saved(F([](auto& s) { s.custom_providers = {local_provider()}; }))},
        {{l::HarnessModel{0, "o3"}, l::HarnessEffort{0, "high"}, l::Save{}}, saved(F([](auto& s) { s.harnesses[0].model = "o3", s.harnesses[0].effort = "high"; }))},
        {{l::HarnessEffort{0, "high"}, l::HarnessEffort{0, std::string(HARNESS_DEFAULT)}, l::RemoveRole{0}, l::Save{}}, saved(F([](auto& s) { s.roles.clear(); }))},
        {{l::ModelToggled{0, false}, l::RenameRole{0, "smol"}, l::Revert{}}, vi::Reverted{}},
    };
    for (std::size_t i = 0; i < cases.size(); ++i) {
        auto s = state();
        CHECK_MESSAGE(run(s, cases[i].first) == cases[i].second, "case " << i);
        CHECK_MESSAGE(s.local_errors().empty() == cases[i].second.has_value(), "case " << i);
    }
}

TEST_CASE("save emits override fields in OVERRIDE_FIELDS order, unset ones left out") {
    auto s = state();
    auto e = run(s, {l::OverrideEdit{0, "preferWebsockets", "false"}, l::OverrideEdit{0, "name", "GPT"}, l::OverrideEdit{0, "contextWindow", ""}, l::Save{}});
    REQUIRE(e.has_value());
    const auto& written = std::get<vi::Save>(*e).settings;
    CHECK(written.overrides[0].fields == std::vector<OverrideField>{{"name", FieldValue{std::string("GPT")}}, {"preferWebsockets", FieldValue{false}}});
}

TEST_CASE("bad override values report every problem by selector and key") {
    auto s = state();
    CHECK_FALSE(run(s, {l::OverrideEdit{0, "reasoning", "maybe"}, l::OverrideEdit{0, "maxTokens", "0"}, l::Save{}}).has_value());
    CHECK(std::vector<std::string>(s.local_errors().begin(), s.local_errors().end()) ==
          std::vector<std::string>{"openai/gpt-5: maxTokens: \"0\" is not a whole number above zero", "openai/gpt-5: reasoning: \"maybe\" is not true or false"});
}

TEST_CASE("custom providers refuse bad ids and pasted keys") {
    auto other = [](void (*f)(CustomProvider&)) {
        auto p = local_provider();
        f(p);
        return p;
    };
    const std::vector<CustomProvider> none, same_id{local_provider()};
    const std::vector<std::tuple<CustomProvider, const std::vector<CustomProvider>*, std::vector<std::string>>> cases{
        {local_provider(), &none, {}},
        {other([](auto& p) { p.api_key_env = "sk-ant-api03-abcdef"; }), &none, {"api_key_env"}},
        {other([](auto& p) { p.api_key_env = "1KEY"; }), &none, {"api_key_env"}},
        {other([](auto& p) { p.id = "My Provider"; }), &none, {"id"}},
        {local_provider(), &same_id, {"id"}},
    };
    for (const auto& [p, others, want] : cases) {
        std::vector<std::string> keys;
        for (const auto& [k, _] : provider_errors(p, *others)) keys.push_back(k);
        CHECK_MESSAGE(keys == want, p.id);
    }
}

TEST_CASE("baseline and errors follow saves and reverts") {
    const ValidationError error{"modelRoles.default", "unknown model"};
    auto s = state();
    run(s, {l::ModelToggled{0, false}});
    CHECK(s.is_dirty());
    run(s, {l::ModelToggled{0, true}});
    CHECK_FALSE(s.is_dirty());
    for (const Local clear : {Local{l::Save{}}, Local{l::Revert{}}}) {
        s.set_errors({error});
        run(s, {l::ProviderToggled{2, true}});
        CHECK_MESSAGE(s.errors().size() == 1, "edits keep the app's errors");
        run(s, {clear});
        CHECK(s.errors().empty());
    }
    auto e = run(s, {l::ProviderToggled{2, true}, l::Save{}});
    REQUIRE(e.has_value());
    const auto written = std::get<vi::Save>(*e).settings;
    CHECK_MESSAGE(s.is_dirty(), "a save waits for the app");
    s.set_errors({error});
    s.saved(written);
    CHECK_FALSE(s.is_dirty());
    CHECK(s.errors().empty());
    CHECK(s.draft_settings() == written);
}

TEST_CASE("the save button writes the edited settings") {
    auto s = state();
    run(s, {l::ModelToggled{0, false}});
    (void)test::app();
    SettingsView view;
    std::vector<Intent> events;
    for (auto m : test::click<SettingsView, State, Message>(view, s, "Save"))
        if (auto e = s.update(m)) events.push_back(*e);
    REQUIRE(events.size() == 1);
    CHECK(events[0] == *saved([](auto& s) { s.models[0].enabled = false; }));
}
