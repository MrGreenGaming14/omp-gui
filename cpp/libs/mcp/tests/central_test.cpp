// Port of `central.rs`'s test, plus the `mcp_servers` table against the Rust golden config.

#include <doctest/doctest.h>

#include <fstream>
#include <sstream>

#include "loa/mcp/central.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::mcp;
using namespace loa::mcp::central;
namespace app = loa::settings::app;

namespace {

std::string slurp(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::ostringstream out;
    out << in.rdbuf();
    return out.str();
}

} // namespace

TEST_CASE("the central config stores servers without touching other keys") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto store = app::ConfigStore::in_dir(dir->path());
    auto config = store.load();
    REQUIRE(config);
    config->harness.default_ = "claude";
    REQUIRE(store.save(*config));

    const ConfigServers servers(store);
    CHECK(servers.load()->empty());
    auto fs = *ServerSpec::make("fs", *StdioSpec::parse("K=v mcp-fs /tmp"));
    auto web = *ServerSpec::make("web", *HttpSpec::make("https://mcp.example.com/mcp"));
    web.enabled = false;
    REQUIRE(servers.save({fs, web}));
    CHECK(*servers.load() == std::vector{fs, web});
    CHECK(store.load()->harness.default_ == "claude");

    const auto session = session_spec(fs);
    const auto* stdio = std::get_if<loa::contracts::McpStdio>(&session.transport);
    REQUIRE(stdio);
    CHECK(stdio->command == "mcp-fs");
    CHECK(stdio->env.at("K") == "v");
    CHECK(from_session_spec(session) == fs);

    app::ProjectSettings settings;
    settings.disabled_tools.insert("bash");
    settings.disabled_mcp_tools["git"].insert("git_push");
    const ProjectFilter filter(settings);
    CHECK(!filter.allows("git", "git_push"));
    CHECK(filter.allows("git", "git_log"));
    CHECK(session_disabled_tools(settings) == std::vector<std::string>{"bash", "mcp__git__git_push"});
    CHECK(harness_tool_name("s", "t") == "mcp__s__t");
}

TEST_CASE("the mcp_servers table round-trips the golden config byte for byte") {
    const std::filesystem::path golden = std::filesystem::path(LOA_GOLDEN_DIR) / "settings";
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);

    // The servers as the Rust app saved them.
    const auto source = app::ConfigStore(golden / "config.saved.toml");
    auto specs = ConfigServers(source).load();
    REQUIRE(specs);
    REQUIRE(specs->size() == 3);
    CHECK((*specs)[0].name == "files");
    CHECK(!(*specs)[1].enabled);
    CHECK(std::get<HttpSpec>((*specs)[2].transport).headers.at("X-Team") == "a");

    // The same config without them, then the table written back through the store port.
    auto config = source.load();
    REQUIRE(config);
    config->mcp_servers.clear();
    const auto target = app::ConfigStore::in_dir(dir->path());
    REQUIRE(target.save(*config));
    REQUIRE(ConfigServers(target).save(*specs));
    CHECK(slurp(target.path()) == slurp(golden / "config.saved.toml"));
}
