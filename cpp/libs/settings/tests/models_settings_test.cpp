// Port of the tests in crates/loa-settings/src/models/settings.rs (the `#[ignore]`d
// golden writer is not ported; goldens are checked elsewhere).

#include <doctest/doctest.h>

#include <algorithm>
#include <cctype>
#include <fstream>
#include <memory>
#include <string>
#include <tuple>

#include "loa/runtime/tempfile.hpp"
#include "loa/settings/models/settings.hpp"

using namespace loa::settings::models::settings;
using loa::settings::models::config::OmpConfig;
using loa::settings::models::config::ScopedEntry;
using loa::settings::models::config::ScopedList;
using loa::settings::models::models_yml::ModelsYml;
namespace rpc = loa::settings::models::rpc;

namespace {

const std::string CONFIG = "# my omp settings\n"
                           "modelRoles:\n"
                           "  default: anthropic/claude-x:high\n"
                           "  smol: \"@default\"\n"
                           "retry:\n"
                           "  fallbackChains:\n"
                           "    default:\n"
                           "      - openai/gpt-y\n"
                           "modelProviderOrder:\n"
                           "  - openai\n"
                           "enabledModels:\n"
                           "  - anthropic/*\n"
                           "  - path: ~/work\n"
                           "    models:\n"
                           "      - openai/gpt-y\n"
                           "disabledProviders:\n"
                           "  - ollama\n"
                           "  - paths:\n"
                           "      - ~/secret\n"
                           "    providers:\n"
                           "      - openai\n"
                           "theme:\n"
                           "  dark: titanium\n";

const std::string MODELS = "providers:\n"
                           "  gw:\n"
                           "    baseUrl: https://gw.example.com/v1\n"
                           "    api: openai-completions\n"
                           "    apiKey: \"!op read op://dev/gw\"\n"
                           "    headers:\n"
                           "      X-Team: core\n"
                           "    models:\n"
                           "      - id: m1\n"
                           "        name: Model One\n"
                           "        contextWindow: 64000\n"
                           "  anthropic:\n"
                           "    modelOverrides:\n"
                           "      claude-x:\n"
                           "        name: Claude X (team)\n"
                           "        contextWindow: 200000\n"
                           "        cost:\n"
                           "          input: 1\n"
                           "          output: 2\n";

rpc::ModelInfo model(std::string provider, std::string id) {
    rpc::ModelInfo m;
    m.name = id;
    std::transform(m.name.begin(), m.name.end(), m.name.begin(), [](unsigned char c) { return std::toupper(c); });
    m.id = std::move(id);
    m.provider = std::move(provider);
    return m;
}

std::vector<rpc::ModelInfo> available() {
    return {model("anthropic", "claude-x"), model("anthropic", "claude-z"), model("openai", "gpt-y")};
}

struct Files {
    std::shared_ptr<loa::runtime::TempDir> dir;
    OmpConfig config;
    ModelsYml models;
};

void spit(const std::filesystem::path& path, const std::string& text) {
    std::ofstream out(path, std::ios::binary);
    out << text;
}

Files files(const std::string& config, const std::string& models) {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    spit(dir->path() / "config.yml", config);
    spit(dir->path() / "models.yml", models);
    auto c = OmpConfig::load(dir->path() / "config.yml");
    REQUIRE(c);
    auto m = ModelsYml::load(dir->path() / "models.yml");
    REQUIRE(m);
    return Files{std::make_shared<loa::runtime::TempDir>(std::move(*dir)), std::move(*c), std::move(*m)};
}

std::unordered_map<std::string, std::string> names() { return {{"anthropic", "Anthropic"}}; }

ModelSettings read_with(const Files& f, const std::vector<rpc::ModelInfo>& models) {
    auto s = settings(f.config, f.models, models, names());
    REQUIRE(s);
    return *s;
}

ModelSettings read(const Files& f) { return read_with(f, available()); }

std::expected<Edited, std::vector<ValidationError>> save(Files& f, const ModelSettings& before, const ModelSettings& after) {
    return apply_settings(f.config, f.models, before, after);
}

bool contains(const std::string& text, std::string_view needle) { return text.find(needle) != std::string::npos; }

std::vector<ScopedEntry> first_two(const std::vector<ScopedEntry>& entries) {
    REQUIRE(entries.size() >= 2);
    return {entries.begin(), entries.begin() + 2};
}

} // namespace

TEST_CASE("reads roles toggles providers overrides and custom providers") {
    const Files f = files(CONFIG, MODELS);
    const ModelSettings s = read(f);
    CHECK(s.roles == std::vector<RoleChain>{
                         RoleChain{"default", {"anthropic/claude-x:high", "openai/gpt-y"}},
                         RoleChain{"smol", {"@default"}},
                     });
    std::vector<std::pair<std::string, bool>> toggles;
    for (const auto& t : s.models) toggles.emplace_back(t.selector, t.enabled);
    CHECK(toggles == std::vector<std::pair<std::string, bool>>{
                         {"anthropic/claude-x", true}, {"anthropic/claude-z", true}, {"openai/gpt-y", false}});
    std::vector<std::tuple<std::string, std::string, bool>> providers;
    for (const auto& p : s.providers) providers.emplace_back(p.id, p.name, p.enabled);
    CHECK(providers == std::vector<std::tuple<std::string, std::string, bool>>{
                           {"openai", "", true}, {"anthropic", "Anthropic", true}, {"ollama", "", false}});
    CHECK(s.overrides == std::vector<ModelOverride>{ModelOverride{
                             "anthropic/claude-x",
                             {
                                 OverrideField{"name", FieldValue::text("Claude X (team)")},
                                 OverrideField{"contextWindow", FieldValue::integer(200'000)},
                             },
                         }});
    CHECK(s.custom_providers == std::vector<CustomProvider>{CustomProvider{
                                    .id = "gw",
                                    .base_url = "https://gw.example.com/v1",
                                    .api = "openai-completions",
                                    .auth = AuthMode::ApiKey,
                                    .api_key_env = "",
                                    .models = {"m1"},
                                }});
}

TEST_CASE("saving an unchanged screen edits nothing") {
    Files f = files(CONFIG, MODELS);
    const ModelSettings s = read(f);
    auto r = save(f, s, s);
    REQUIRE(r);
    CHECK(*r == Edited{});
    CHECK(f.config.text() == CONFIG);
    CHECK(f.models.text() == MODELS);
}

TEST_CASE("role edits touch only their keys") {
    Files f = files(CONFIG, MODELS);
    const ModelSettings before = read(f);
    ModelSettings after = before;
    after.roles[0].chain.push_back("anthropic/claude-z");
    after.roles.erase(after.roles.begin() + 1);
    after.roles.push_back(RoleChain{"plan", {"openai/gpt-y:low"}});
    auto r = save(f, before, after);
    REQUIRE(r);
    CHECK(*r == Edited{.config = true, .models = false});
    const std::string text = f.config.text();
    INFO(text);
    CHECK(text.starts_with("# my omp settings\nmodelRoles:\n  default: anthropic/claude-x:high\n"));
    CHECK_FALSE(contains(text, "smol"));
    CHECK(contains(text, "plan: openai/gpt-y:low"));
    CHECK(contains(text, "      - openai/gpt-y\n      - anthropic/claude-z\n"));
    CHECK(text.ends_with("theme:\n  dark: titanium\n"));
    CHECK(read(f).roles == after.roles);

    ModelSettings bad = after;
    bad.roles[0].chain[0] = "has space";
    auto errors = save(f, after, bad);
    REQUIRE_FALSE(errors);
    CHECK(errors.error()[0].location == "Model roles › default");
}

TEST_CASE("enabled models keep patterns that still hold and path entries") {
    Files f = files(CONFIG, MODELS);
    {
        const ModelSettings before = read(f);
        ModelSettings after = before;
        after.models[2].enabled = true;
        REQUIRE(save(f, before, after));
        const auto entries = f.config.scoped_list(ScopedList::EnabledModels);
        // Every model enabled: only the path entry is left.
        REQUIRE(entries.size() == 1);
        CHECK_FALSE(entries[0].is_everywhere());
    }

    const ModelSettings before = read(f);
    CHECK(std::all_of(before.models.begin(), before.models.end(), [](const ModelToggle& t) { return t.enabled; }));
    ModelSettings after = before;
    after.models[1].enabled = false;
    REQUIRE(save(f, before, after));
    const auto entries = f.config.scoped_list(ScopedList::EnabledModels);
    CHECK(first_two(entries) == std::vector<ScopedEntry>{ScopedEntry::everywhere("anthropic/claude-x"),
                                                          ScopedEntry::everywhere("openai/gpt-y")});
    REQUIRE(entries.size() >= 3);
    CHECK_FALSE(entries[2].is_everywhere());

    ModelSettings none = after;
    for (auto& t : none.models) t.enabled = false;
    auto errors = save(f, after, none);
    REQUIRE_FALSE(errors);
    CHECK(errors.error()[0].location == "Enabled models");
}

TEST_CASE("glob patterns stay while their models stay enabled") {
    Files f = files(CONFIG, MODELS);
    auto models = available();
    models.push_back(model("openai", "gpt-w"));
    {
        const ModelSettings before = read_with(f, models);
        ModelSettings after = before;
        auto gpt_y = std::find_if(after.models.begin(), after.models.end(),
                                  [](const ModelToggle& t) { return t.selector == "openai/gpt-y"; });
        REQUIRE(gpt_y != after.models.end());
        gpt_y->enabled = true;
        REQUIRE(save(f, before, after));
        const auto entries = f.config.scoped_list(ScopedList::EnabledModels);
        CHECK(first_two(entries) == std::vector<ScopedEntry>{ScopedEntry::everywhere("anthropic/*"),
                                                              ScopedEntry::everywhere("openai/gpt-y")});
    }

    const ModelSettings before = read_with(f, models);
    ModelSettings after = before;
    after.models[1].enabled = false;
    REQUIRE(save(f, before, after));
    const auto entries = f.config.scoped_list(ScopedList::EnabledModels);
    // anthropic/* no longer holds, so its enabled model is listed itself.
    CHECK(first_two(entries) == std::vector<ScopedEntry>{ScopedEntry::everywhere("anthropic/claude-x"),
                                                          ScopedEntry::everywhere("openai/gpt-y")});
}

TEST_CASE("provider order and disabling keep path entries") {
    Files f = files(CONFIG, MODELS);
    const ModelSettings before = read(f);
    ModelSettings after = before;
    std::swap(after.providers[1], after.providers[0]);
    after.providers[2].enabled = true;
    after.providers[1].enabled = false;
    REQUIRE(save(f, before, after));
    CHECK(f.config.provider_order() == std::vector<std::string>{"anthropic", "openai", "ollama"});
    const auto disabled = f.config.scoped_list(ScopedList::DisabledProviders);
    REQUIRE(disabled.size() == 2);
    CHECK_FALSE(disabled[0].is_everywhere());
    CHECK(disabled[1] == ScopedEntry::everywhere("openai"));
}

TEST_CASE("override edits change only shown fields") {
    Files f = files(CONFIG, MODELS);
    const ModelSettings before = read(f);
    ModelSettings after = before;
    after.overrides[0].fields = {
        OverrideField{"contextWindow", FieldValue::integer(300'000)},
        OverrideField{"maxTokens", FieldValue::integer(8'192)},
    };
    after.overrides.push_back(ModelOverride{"openai/gpt-y", {OverrideField{"reasoning", FieldValue::boolean(true)}}});
    auto r = save(f, before, after);
    REQUIRE(r);
    CHECK(*r == Edited{.config = false, .models = true});
    const std::string text = f.models.text();
    INFO(text);
    CHECK_FALSE(contains(text, "Claude X (team)"));
    CHECK(contains(text, "contextWindow: 300000"));
    CHECK(contains(text, "maxTokens: 8192"));
    CHECK(contains(text, "cost:")); // unshown fields stay
    CHECK(contains(text, "  openai:\n    modelOverrides:\n      gpt-y:\n        reasoning: true\n"));
    CHECK(read(f).overrides == after.overrides);

    ModelSettings bad = after;
    bad.overrides[0].fields.push_back(OverrideField{"temperature", FieldValue::real(0.2)});
    auto errors = save(f, after, bad);
    REQUIRE_FALSE(errors);
    CHECK(errors.error()[0].location == "Model overrides › anthropic/claude-x");
    CHECK(contains(errors.error()[0].message, "temperature"));
}

TEST_CASE("custom provider edits keep secret commands headers and model details") {
    Files f = files(CONFIG, MODELS);
    {
        const ModelSettings before = read(f);
        ModelSettings after = before;
        after.custom_providers[0].base_url = "https://gw2.example.com/v1";
        after.custom_providers[0].models.push_back("m2");
        after.custom_providers.push_back(CustomProvider{
            .id = "local",
            .base_url = "http://127.0.0.1:8080/v1",
            .api = "openai-completions",
            .auth = AuthMode::ApiKey,
            .api_key_env = "LOCAL_KEY",
            .models = {"qwen"},
        });
        REQUIRE(save(f, before, after));
        const std::string text = f.models.text();
        INFO(text);
        CHECK(contains(text, "baseUrl: https://gw2.example.com/v1"));
        CHECK(contains(text, "apiKey: \"!op read op://dev/gw\""));
        CHECK(contains(text, "X-Team: core"));
        CHECK(contains(text, "name: Model One"));
        CHECK(contains(text, "- id: m2"));
        CHECK(contains(text, "  local:\n    baseUrl: http://127.0.0.1:8080/v1\n"));
        CHECK(contains(text, "apiKey: LOCAL_KEY"));
        CHECK_FALSE(contains(text, "auth:"));
    }

    const ModelSettings before = read(f);
    {
        ModelSettings after = before;
        after.custom_providers[0].api_key_env = "sk-pasted-secret";
        Files copy = f;
        auto errors = save(copy, before, after);
        REQUIRE_FALSE(errors);
        CHECK(errors.error().size() == 1);
        CHECK(errors.error()[0].location == "Custom providers › gw");
    }

    ModelSettings after = before;
    std::erase_if(after.custom_providers, [](const CustomProvider& p) { return p.id == "gw"; });
    after.custom_providers[0].api_key_env = "";
    after.custom_providers[0].auth = AuthMode::None;
    REQUIRE(save(f, before, after));
    const std::string text = f.models.text();
    INFO(text);
    CHECK_FALSE(contains(text, "gw"));
    CHECK_FALSE(contains(text, "apiKey")); // clearing the variable removes the key
    CHECK(contains(text, "    auth: none\n"));
    CHECK(contains(text, "  anthropic:\n    modelOverrides:"));
}

TEST_CASE("globs match selectors and bare ids") {
    CHECK(pattern_matches("anthropic/*", "anthropic", "claude-x"));
    CHECK(pattern_matches("claude-?", "anthropic", "claude-x"));
    CHECK(pattern_matches("claude-x", "anthropic", "claude-x"));
    CHECK_FALSE(pattern_matches("openai/*", "anthropic", "claude-x"));
    CHECK_FALSE(pattern_matches("anthropic/claude", "anthropic", "claude-x"));
}

namespace {
std::string slurp_file(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    REQUIRE(in);
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}
const std::filesystem::path GOLDEN_SETTINGS = std::filesystem::path(LOA_GOLDEN_DIR) / "settings";
} // namespace

// The edit the Rust `write_golden_yml` writer saves into `cpp/tests/golden/settings`.
TEST_CASE("role edit saves the golden config.yml and models.yml byte for byte") {
    Files f = files(CONFIG, MODELS);
    const ModelSettings before = read(f);
    ModelSettings after = before;
    after.roles[0].chain.push_back("anthropic/claude-z");
    after.roles.push_back(RoleChain{"plan", {"openai/gpt-y:low"}});
    REQUIRE(save(f, before, after));
    REQUIRE(f.config.save());
    REQUIRE(f.models.save());
    CHECK(slurp_file(f.config.path()) == slurp_file(GOLDEN_SETTINGS / "config.yml"));
    CHECK(slurp_file(f.dir->path() / "models.yml") == slurp_file(GOLDEN_SETTINGS / "models.yml"));
}

TEST_CASE("golden config.yml and models.yml round trip through load, read and save") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const std::string config = slurp_file(GOLDEN_SETTINGS / "config.yml");
    const std::string models = slurp_file(GOLDEN_SETTINGS / "models.yml");
    Files f = files(config, models);
    const ModelSettings s = read(f);
    CHECK(s.roles.size() == 3);
    CHECK(s.roles[0].chain == std::vector<std::string>{"anthropic/claude-x:high", "openai/gpt-y", "anthropic/claude-z"});
    // Saving the unchanged screen edits nothing; rewriting every role with its own value
    // keeps the bytes.
    REQUIRE(save(f, s, s));
    for (const auto& role : s.roles) {
        auto assigned = f.config.model_role(role.role);
        REQUIRE(assigned);
        REQUIRE(*assigned);
        REQUIRE(f.config.set_model_role(role.role, (*assigned)->candidates));
    }
    REQUIRE(f.config.save());
    REQUIRE(f.models.save());
    CHECK(slurp_file(f.config.path()) == config);
    CHECK(slurp_file(f.dir->path() / "models.yml") == models);
}
