// Port of the `models/config.rs` tests, plus `save_validated` through a fake validator.

#include <doctest/doctest.h>

#include <filesystem>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

#include <asio/co_spawn.hpp>
#include <asio/io_context.hpp>
#include <asio/use_future.hpp>

#include "loa/runtime/tempfile.hpp"
#include "loa/settings/models/config.hpp"

using namespace loa::settings::models;
using namespace loa::settings::models::config;
namespace fs = std::filesystem;

namespace {

const std::string CONFIG = "# Global omp settings\n"
                           "theme:\n"
                           "  dark: titanium\n"
                           "\n"
                           "modelRoles:\n"
                           "  default: anthropic/claude-sonnet-4-5 # day to day\n"
                           "  slow: anthropic/claude-opus-4-5:high\n"
                           "  plan: \"@slow\"\n"
                           "\n"
                           "retry:\n"
                           "  maxRetries: 5 # keep this\n"
                           "  fallbackChains:\n"
                           "    default:\n"
                           "      - openai/gpt-5\n"
                           "    judge: []\n"
                           "\n"
                           "disabledProviders:\n"
                           "  - ollama\n"
                           "  - path: ~/private\n"
                           "    providers: [anthropic]\n";

std::string slurp(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::ostringstream s;
    s << in.rdbuf();
    return s.str();
}

void spit(const fs::path& path, std::string_view text) {
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    out << text;
}

bool contains(const std::string& s, std::string_view part) { return s.find(part) != std::string::npos; }

struct Loaded {
    loa::runtime::TempDir dir;
    OmpConfig cfg;
};

Loaded load(std::string_view text) {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto path = dir->path() / "config.yml";
    spit(path, text);
    auto cfg = OmpConfig::load(path);
    REQUIRE(cfg);
    return Loaded{std::move(*dir), std::move(*cfg)};
}

Selector parse(std::string_view text) {
    auto s = Selector::parse(text);
    REQUIRE(s);
    return *s;
}

} // namespace

TEST_CASE("reads roles with thinking suffixes and aliases") {
    auto l = load(CONFIG);
    auto roles = l.cfg.model_roles();
    REQUIRE(roles);
    REQUIRE(roles->size() == 3);
    CHECK((*roles)[1].candidates == std::vector{Selector::model("anthropic/claude-opus-4-5", "high")});
    CHECK((*roles)[2].candidates == std::vector{Selector::role("slow")});
}

TEST_CASE("assigning a role rewrites only that line and saves atomically") {
    auto l = load(CONFIG);
    const std::vector candidates = {parse("openai/gpt-5:medium"), Selector::model("anthropic/claude-sonnet-4-5")};
    REQUIRE(l.cfg.set_model_role("default", candidates));
    REQUIRE(l.cfg.save());
    std::string expected = CONFIG;
    const std::string_view from = "  default: anthropic/claude-sonnet-4-5 # day to day";
    expected.replace(expected.find(from), from.size(), "  default: openai/gpt-5:medium,anthropic/claude-sonnet-4-5 # day to day");
    CHECK(slurp(l.dir.path() / "config.yml") == expected);
    auto again = OmpConfig::load(l.dir.path() / "config.yml");
    REQUIRE(again);
    auto role = again->model_role("default");
    REQUIRE(role);
    REQUIRE(*role);
    CHECK((*role)->candidates.size() == 2);
}

TEST_CASE("new role is quoted when it starts with at") {
    auto l = load(CONFIG);
    const std::vector candidates = {Selector::role("smol")};
    REQUIRE(l.cfg.set_model_role("commit", candidates));
    CHECK(contains(l.cfg.text(), "  plan: \"@slow\"\n  commit: \"@smol\"\n"));
    auto role = l.cfg.model_role("commit");
    REQUIRE(role);
    REQUIRE(*role);
    CHECK((*role)->candidates == candidates);
}

TEST_CASE("kind roles refuse thinking suffixes") {
    auto l = load(CONFIG);
    const std::vector candidates = {parse("typesafe/jev-1.14.0:high")};
    auto err = l.cfg.set_model_role("judge", candidates);
    REQUIRE_FALSE(err);
    CHECK(err.error().kind == Error::Kind::Invalid);
    CHECK(l.cfg.text() == CONFIG);
}

TEST_CASE("selectors with commas or spaces are rejected") {
    CHECK_FALSE(Selector::parse("a/b c"));
    CHECK_FALSE(Selector::parse("@"));
    CHECK_FALSE(Selector::model("a,b").validate());
}

TEST_CASE("clearing the only role removes modelRoles entirely") {
    auto l = load("modelRoles:\n  default: a/b\nother: 1\n");
    auto cleared = l.cfg.clear_model_role("default");
    REQUIRE(cleared);
    CHECK(*cleared);
    CHECK(l.cfg.text() == "other: 1\n");
}

TEST_CASE("fallback chains keep empty lists and sibling retry settings") {
    auto l = load(CONFIG);
    auto chains = l.cfg.fallback_chains();
    REQUIRE(chains);
    CHECK((*chains)[1].key == ChainKey::role("judge"));
    CHECK((*chains)[1].fallbacks.empty());

    auto key = ChainKey::parse("google-antigravity/*");
    REQUIRE(key);
    const std::vector fallbacks = {Selector::model("google/*"), Selector::role("smol")};
    REQUIRE(l.cfg.set_fallback_chain(*key, fallbacks));
    REQUIRE(l.cfg.set_fallback_chain(ChainKey::role("default"), {}));
    CHECK(contains(l.cfg.text(), "  maxRetries: 5 # keep this\n"));
    CHECK(contains(l.cfg.text(), "    default: []\n    judge: []\n    google-antigravity/*:\n      - google/*\n      - \"@smol\"\n"));
    chains = l.cfg.fallback_chains();
    REQUIRE(chains);
    CHECK((*chains)[2].key == ChainKey::provider("google-antigravity"));
    CHECK((*chains)[2].fallbacks.size() == 2);
}

TEST_CASE("removing the last chain keeps other retry settings") {
    auto l = load("retry:\n  maxRetries: 5\n  fallbackChains:\n    smol: [a/b]\n");
    REQUIRE(l.cfg.clear_fallback_chain(ChainKey::role("smol")));
    CHECK(l.cfg.text() == "retry:\n  maxRetries: 5\n");
}

TEST_CASE("chain keys parse like Rust") {
    CHECK(ChainKey::parse("default") == ChainKey::role("default"));
    CHECK(ChainKey::parse("openai/gpt-5") == ChainKey::model("openai/gpt-5"));
    CHECK(ChainKey::parse("openai/*") == ChainKey::provider("openai"));
    CHECK_FALSE(ChainKey::parse("/*"));
    CHECK_FALSE(ChainKey::parse("a b/c"));
    CHECK_FALSE(ChainKey::parse("not a role"));
}

TEST_CASE("scoped lists round trip with path entries") {
    auto l = load(CONFIG);
    const auto disabled = l.cfg.scoped_list(ScopedList::DisabledProviders);
    CHECK(disabled == std::vector{ScopedEntry::everywhere("ollama"), ScopedEntry::scoped({"~/private"}, {"anthropic"})});
    const std::vector entries = {ScopedEntry::everywhere("claude-sonnet-4-5"),
                                 ScopedEntry::scoped({"~/work", "~/oss"}, {"anthropic/claude-opus-4-5"})};
    REQUIRE(l.cfg.set_scoped_list(ScopedList::EnabledModels, entries));
    CHECK(l.cfg.text().ends_with(
        "enabledModels:\n  - claude-sonnet-4-5\n  - paths:\n      - ~/work\n      - ~/oss\n    models:\n      - anthropic/claude-opus-4-5\n"));
    CHECK(l.cfg.scoped_list(ScopedList::EnabledModels).size() == 2);
    CHECK(l.cfg.scoped_list(ScopedList::DisabledProviders) == disabled);
}

TEST_CASE("path scoped entries need a path and a value") {
    auto l = load(CONFIG);
    const std::vector entries = {ScopedEntry::scoped({}, {"x"})};
    auto err = l.cfg.set_scoped_list(ScopedList::EnabledModels, entries);
    REQUIRE_FALSE(err);
    CHECK(err.error().to_string() == "invalid value for enabledModels: a path scoped entry needs at least one path and one value");
}

TEST_CASE("emptying a list removes the key") {
    auto l = load(CONFIG);
    REQUIRE(l.cfg.set_scoped_list(ScopedList::DisabledProviders, {}));
    CHECK_FALSE(contains(l.cfg.text(), "disabledProviders"));
    CHECK(contains(l.cfg.text(), "judge: []"));
}

TEST_CASE("provider order rejects duplicates and writes a list") {
    auto l = load(CONFIG);
    const std::vector<std::string> dup = {"anthropic", "anthropic"};
    CHECK_FALSE(l.cfg.set_provider_order(dup));
    const std::vector<std::string> order = {"anthropic", "openai"};
    REQUIRE(l.cfg.set_provider_order(order));
    CHECK(l.cfg.provider_order() == order);
    CHECK(l.cfg.text().ends_with("modelProviderOrder:\n  - anthropic\n  - openai\n"));
}

TEST_CASE("missing file is created on save") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto path = dir->path() / "agent" / "config.yml";
    auto cfg = OmpConfig::load(path);
    REQUIRE(cfg);
    const std::vector candidates = {Selector::model("openai/gpt-5")};
    REQUIRE(cfg->set_model_role("default", candidates));
    auto saved = cfg->save();
    REQUIRE_MESSAGE(saved.has_value(), (saved ? "" : saved.error().to_string()));
    CHECK(slurp(path) == "modelRoles:\n  default: openai/gpt-5\n");
}

TEST_CASE("a file changed on disk is not overwritten") {
    auto l = load(CONFIG);
    const std::vector candidates = {Selector::model("openai/gpt-5")};
    REQUIRE(l.cfg.set_model_role("default", candidates));
    spit(l.dir.path() / "config.yml", "theme: {}\n");
    auto saved = l.cfg.save();
    REQUIRE_FALSE(saved);
    CHECK(saved.error().kind == Error::Kind::ChangedOnDisk);
}

namespace {
struct FakeValidator final : SettingsValidator {
    mutable std::vector<std::pair<std::string, loa::runtime::json::Value>> seen;
    bool reject = false;
    loa::contracts::Awaitable<Result<>> check_config(std::string, std::vector<std::pair<std::string, loa::runtime::json::Value>> expected) const override {
        seen = std::move(expected);
        if (reject) {
            co_return std::unexpected(Error::rejected("config.yml", "no"));
        }
        co_return Result<>{};
    }
    loa::contracts::Awaitable<Result<>> check_models(std::string) const override { co_return Result<>{}; }
};
} // namespace

TEST_CASE("save_validated sends the touched settings and saves only when accepted") {
    auto l = load(CONFIG);
    const std::vector candidates = {Selector::model("openai/gpt-5")};
    REQUIRE(l.cfg.set_model_role("default", candidates));
    REQUIRE(l.cfg.set_fallback_chain(ChainKey::role("smol"), candidates));
    CHECK(l.cfg.is_edited());
    FakeValidator validator;
    validator.reject = true;
    asio::io_context io;
    auto rejected = asio::co_spawn(io, l.cfg.save_validated(validator), asio::use_future);
    io.run();
    CHECK_FALSE(rejected.get());
    CHECK(slurp(l.dir.path() / "config.yml") == CONFIG);
    REQUIRE(validator.seen.size() == 2);
    CHECK(validator.seen[0].first == "modelRoles");
    CHECK(validator.seen[1].first == "retry.fallbackChains");
    CHECK(loa::runtime::json::dump(validator.seen[1].second) == R"({"default":["openai/gpt-5"],"judge":[],"smol":["openai/gpt-5"]})");

    validator.reject = false;
    io.restart();
    auto accepted = asio::co_spawn(io, l.cfg.save_validated(validator), asio::use_future);
    io.run();
    CHECK(accepted.get());
    CHECK_FALSE(l.cfg.is_edited());
    CHECK(slurp(l.dir.path() / "config.yml") == l.cfg.text());
}
