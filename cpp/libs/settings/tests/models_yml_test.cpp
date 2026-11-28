// Port of the tests in crates/loa-settings/src/models/models_yml.rs.

#include <doctest/doctest.h>

#include <fstream>
#include <string>

#include "loa/runtime/tempfile.hpp"
#include "loa/settings/models/models_yml.hpp"

using namespace loa::settings::models::models_yml;

namespace {

const std::string MODELS = "# Custom providers for this machine\n"
                           "providers:\n"
                           "  # Corporate OpenRouter relay\n"
                           "  openrouter:\n"
                           "    baseUrl: https://my-proxy.example.com/v1\n"
                           "    headers:\n"
                           "      X-Team: platform\n"
                           "    modelOverrides:\n"
                           "      anthropic/claude-sonnet-4:\n"
                           "        name: Sonnet 4 (Corp)\n"
                           "        compat:\n"
                           "          openRouterRouting:\n"
                           "            only: [anthropic]\n"
                           "\n"
                           "  local-openai:\n"
                           "    baseUrl: http://127.0.0.1:8000/v1\n"
                           "    auth: none\n"
                           "    api: openai-completions\n"
                           "    models:\n"
                           "      - id: Qwen/Qwen2.5-Coder-32B-Instruct\n"
                           "        name: Qwen 2.5 Coder 32B (local)\n"
                           "\n"
                           "  pinned:\n"
                           "    baseUrl: https://gw.example.com/v1\n"
                           "    api: openai-completions\n"
                           "    apiKey: sk-live-0123456789abcdef\n"
                           "    models:\n"
                           "      - id: fast\n";

struct Loaded {
    loa::runtime::TempDir dir;
    ModelsYml m;
};

Loaded load(const std::string& text) {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto path = dir->path() / "models.yml";
    {
        std::ofstream out(path, std::ios::binary);
        out << text;
    }
    auto m = ModelsYml::load(path);
    REQUIRE(m);
    return Loaded{std::move(*dir), std::move(*m)};
}

bool contains(const std::string& text, std::string_view needle) { return text.find(needle) != std::string::npos; }

} // namespace

TEST_CASE("reads providers overrides and never reports an inline key") {
    auto [_d, m] = load(MODELS);
    const auto providers = m.providers();
    REQUIRE(providers.size() == 3);
    const auto& o = providers[0];
    CHECK(o.overrides[0].first == "anthropic/claude-sonnet-4");
    CHECK(o.overrides[0].second.name == std::optional<std::string>("Sonnet 4 (Corp)"));
    CHECK(o.overrides[0].second.other == std::vector<std::string>{"compat"});
    CHECK(providers[1].auth == std::optional(AuthMode::None));
    CHECK(providers[1].models[0].id == "Qwen/Qwen2.5-Coder-32B-Instruct");
    CHECK(providers[2].api_key == std::optional(ApiKeySetting::literal()));
}

TEST_CASE("setting an override keeps compat comments and other providers") {
    auto [_d, m] = load(MODELS);
    REQUIRE(m.set_override("openrouter", "anthropic/claude-sonnet-4", OverrideField::context_window(200'000)));
    std::string expected = MODELS;
    const std::string from = "            only: [anthropic]\n";
    expected.replace(expected.find(from), from.size(), "            only: [anthropic]\n        contextWindow: 200000\n");
    CHECK(m.text() == expected);
}

TEST_CASE("override for a builtin provider creates the provider entry") {
    auto [_d, m] = load(MODELS);
    REQUIRE(m.set_override("openai-codex", "gpt-5.5", OverrideField::context_promotion_target("openai-codex/gpt-5.4")));
    CHECK(m.text().ends_with("      - id: fast\n  openai-codex:\n    modelOverrides:\n      gpt-5.5:\n        "
                             "contextPromotionTarget: openai-codex/gpt-5.4\n"));
    CHECK(m.text().starts_with("# Custom providers for this machine\n"));
}

TEST_CASE("clearing the only override removes the override only provider") {
    auto [_d, m] = load("providers:\n  openai:\n    modelOverrides:\n      gpt-5:\n        preferWebsockets: false\n");
    auto removed = m.clear_override("openai", "gpt-5", OverrideKey::PreferWebsockets);
    REQUIRE(removed);
    CHECK(*removed);
    CHECK(m.text() == "");
}

TEST_CASE("override values are written without cross field validation") {
    auto [_d, m] = load(MODELS);
    REQUIRE(m.set_override("openrouter", "x/y", OverrideField::context_window(200'000)));
    REQUIRE(m.set_override("openrouter", "x/y", OverrideField::max_context_window(100'000)));
    CHECK(contains(m.text(), "maxContextWindow: 100000"));
}

TEST_CASE("custom provider with models round trips") {
    auto [_d, m] = load("");
    CustomProvider p{
        .id = "my-gateway",
        .api = "openai-completions",
        .models = {CustomModel{.id = "claude-sonnet", .context_window = 200'000, .max_tokens = 8192}},
    };
    REQUIRE(m.set_custom_provider(p));
    p.base_url = "https://gateway.example.com/v1";
    REQUIRE(m.set_custom_provider(p));
    auto key = KeyReference::env_var("MY_GATEWAY_API_KEY");
    REQUIRE(key);
    p.api_key = ApiKeyWrite::set(*key);
    REQUIRE(m.set_custom_provider(p));
    CHECK(m.text() == "providers:\n  my-gateway:\n    api: openai-completions\n    models:\n      - id: claude-sonnet\n  "
                      "      contextWindow: 200000\n        maxTokens: 8192\n    baseUrl: "
                      "https://gateway.example.com/v1\n    apiKey: MY_GATEWAY_API_KEY\n");
    const auto read = m.provider("my-gateway");
    REQUIRE(read);
    CHECK(read->api_key == std::optional(ApiKeySetting::env_var("MY_GATEWAY_API_KEY")));
}

TEST_CASE("updating a provider keeps its headers and overrides") {
    auto [_d, m] = load(MODELS);
    auto key = KeyReference::command("!op read op://dev/openrouter/key");
    REQUIRE(key);
    const CustomProvider p{
        .id = "openrouter",
        .base_url = "https://relay2.example.com/v1",
        .api_key = ApiKeyWrite::set(*key),
    };
    REQUIRE(m.set_custom_provider(p));
    CHECK(contains(m.text(), "    baseUrl: https://relay2.example.com/v1\n    headers:\n      X-Team: platform\n"));
    CHECK(contains(m.text(), "name: Sonnet 4 (Corp)"));
    CHECK(contains(m.text(), "apiKey: \"!op read op://dev/openrouter/key\""));
    const auto read = m.provider("openrouter");
    REQUIRE(read);
    CHECK(read->api_key == std::optional(ApiKeySetting::command()));
}

TEST_CASE("discovery without api is written") {
    auto [_d, m] = load("");
    auto key = KeyReference::env_var("TEAM_PROXY_API_KEY");
    REQUIRE(key);
    CustomProvider p{
        .id = "team-proxy",
        .base_url = "https://models.example.com/v1",
        .api_key = ApiKeyWrite::set(*key),
        .discovery = "openai-models-list",
    };
    REQUIRE(m.set_custom_provider(p));
    p.discovery = "proxy";
    REQUIRE(m.set_custom_provider(p));
    CHECK(contains(m.text(), "    discovery:\n      type: proxy\n"));
}

TEST_CASE("key references refuse pasted keys") {
    CHECK_FALSE(KeyReference::env_var("sk-ant-api03-abcdef"));
    CHECK_FALSE(KeyReference::env_var("my_key"));
    CHECK(KeyReference::env_var("OPENAI_API_KEY"));
    CHECK_FALSE(KeyReference::command(""));
    CHECK_FALSE(KeyReference::command("a\nb"));
}

TEST_CASE("cost overrides round trip fractions") {
    auto [_d, m] = load("");
    const ModelCost cost{.input = 3.0, .output = 15.0, .cache_read = 0.3, .cache_write = 3.75};
    REQUIRE(m.set_override("anthropic", "claude-sonnet-5", OverrideField::cost(cost)));
    CHECK(contains(m.text(), "cost:\n          input: 3\n          output: 15\n          cacheRead: 0.3\n          "
                             "cacheWrite: 3.75\n"));
    const auto read = m.provider("anthropic");
    REQUIRE(read);
    CHECK(read->overrides[0].second.cost == std::optional(cost));
}
