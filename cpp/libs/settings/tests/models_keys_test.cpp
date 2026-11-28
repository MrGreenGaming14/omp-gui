// Port of the tests in crates/loa-settings/src/models/keys.rs.

#include <doctest/doctest.h>

#include <array>
#include <fstream>
#include <set>
#include <string>

#include "loa/runtime/tempfile.hpp"
#include "loa/settings/models/keys.hpp"

using namespace loa::settings::models::keys;
using loa::settings::models::models_yml::ModelsYml;

namespace {

struct Env final : EnvironmentPresence {
    std::set<std::string, std::less<>> names;
    [[nodiscard]] bool contains(std::string_view name) const override { return names.find(name) != names.end(); }
};

} // namespace

TEST_CASE("key sources never include the inline secret or environment value") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto path = dir->path() / "models.yml";
    {
        std::ofstream out(path, std::ios::binary);
        out << "providers:\n  gateway:\n    apiKey: sk-supersecret-example\n  proxy:\n    apiKey: '!op read private'\n";
    }
    auto models = ModelsYml::load(path);
    REQUIRE(models);
    Env env;
    env.names.insert("OPENAI_API_KEY");
    const std::array<std::string_view, 1> openai = {"OPENAI_API_KEY"};
    CHECK(credential_source(*models, "gateway", {}, env) == KeySource::config_entry());
    CHECK(credential_source(*models, "proxy", {}, env) == KeySource::secret_command());
    CHECK(credential_source(*models, "openai", openai, env) == KeySource::environment("OPENAI_API_KEY"));
    // The Rust test checks the `Debug` text; a KeySource carries no value besides `name`.
    CHECK(credential_source(*models, "gateway", {}, env).name.find("sk-supersecret") == std::string::npos);
}
