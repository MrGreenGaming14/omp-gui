#include <doctest/doctest.h>

#include <algorithm>
#include <array>
#include <fstream>
#include <iterator>

#include "loa/harness/codex/adapter.hpp"
#include "loa/harness/registry/registry.hpp"
#include "loa/harness/testing/testing.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/runtime/tempfile.hpp"
#include "../kit/support.hpp"

using namespace loa::harness::codex;
namespace core = loa::core;
namespace contracts = loa::contracts;
namespace json = loa::runtime::json;
using loa::harness::test::block_on;

TEST_CASE("codex: adapter capabilities and registry factory") {
    CodexAdapter adapter("node");
    CHECK(adapter.harness() == "codex");
    CHECK(adapter.executable() == "codex");
    CHECK(adapter.with_executable("/path/codex").executable() == "/path/codex");
    CHECK(adapter.with_args({"--extra"}).args() == std::vector<std::string>{"--extra"});
    const auto caps = adapter.capabilities();
    CHECK(caps.schema_version == core::SCHEMA_VERSION);
    CHECK(caps.has(contracts::Feature::Fork));
    CHECK(caps.has(contracts::Feature::Compaction));
    CHECK(caps.has(contracts::Feature::Subagents));
    CHECK(caps.has(contracts::Feature::PlanMode));
    CHECK(caps.model_switch == contracts::ModelSwitch::PerTurn);
    CHECK(caps.host_tools);
    CHECK(caps.mcp_servers);
    CHECK(caps.effort);
    CHECK(caps.import);
    CHECK(caps.export_);
    register_factory();
    auto entries = loa::harness::registry::factories({"local-node", {}});
    REQUIRE(entries.size() == 6);
    auto built = entries[2]->build(std::nullopt);
    REQUIRE(built);
    CHECK(built->harness() == "codex");
    CHECK(entries[2]->account(std::nullopt));
}

TEST_CASE("codex: fake app-server drives thread turn and paged models") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    std::ifstream script(std::filesystem::path(LOA_HARNESS_FIXTURES) / "codex/driver.script", std::ios::binary);
    REQUIRE(script);
    auto text = std::string(std::istreambuf_iterator<char>(script), std::istreambuf_iterator<char>());
    auto fake = loa::harness::testing::FakeProcess::install(dir->path(), text);
    REQUIRE(fake);
    contracts::SessionConfig config;
    config.session_id = "session-invented";
    config.node = "local-node";
    config.cwd = dir->path();
    config.model = "gpt-invented";
    config.mcp_servers.push_back({"inventory", contracts::McpStdio{"inventory-mcp", {"--stdio"}, {}}});
    auto sink = std::make_shared<core::MemorySink>();
    auto adapter = CodexAdapter("local-node").with_executable(fake->executable());
    auto session = block_on(exec, adapter.start(config, sink));
    REQUIRE(session);
    auto models = block_on(exec, (*session)->models());
    REQUIRE(models);
    REQUIRE(models->size() == 2);
    CHECK(models->at(0) == contracts::HarnessModel{"gpt-invented", "GPT Invented", "openai", {"low", "high"}});
    CHECK(models->at(1) == contracts::HarnessModel{"gpt-mini", "GPT Mini", "openai", {}});
    REQUIRE(block_on(exec, (*session)->set_effort("high")));
    CHECK(sink->dom().root()->str_attr(core::attr::EFFORT) == "high");
    REQUIRE(block_on(exec, (*session)->send(contracts::Prompt{"Ask inventory", {}, "person"})));
    REQUIRE(block_on(exec, (*session)->wait_idle()));
    auto dom = sink->dom();
    CHECK(std::ranges::any_of(dom.iter(), [](const core::Node& n) {
        return n.kind == core::ElementKind::Message && n.text == "Invented answer";
    }));
    auto turns = dom.children_of_kind(dom.root()->id, core::ElementKind::Turn);
    REQUIRE(turns.size() == 1);
    CHECK(turns.front()->str_attr(core::attr::STATUS) == "done");
    auto args = fake->args();
    CHECK(std::ranges::find(args, "mcp_servers.inventory.command=\"inventory-mcp\"") != args.end());
    const std::array<std::string_view, 3> launch{"app-server", "--listen", "stdio://"};
    CHECK(std::search(args.begin(), args.end(), launch.begin(), launch.end()) != args.end());
    auto sent = fake->received();
    bool effort = false;
    bool cursor = false;
    for (const auto& line : sent) {
        const auto* method = line.find("method");
        const auto* params = line.find("params");
        if (!method || !params || !method->as_string()) continue;
        if (*method->as_string() == "turn/start") {
            const auto* value = params->find("effort");
            effort = value && value->as_string() && *value->as_string() == "high";
        }
        if (*method->as_string() == "model/list") {
            const auto* value = params->find("cursor");
            cursor |= value && value->as_string() && *value->as_string() == "page-2";
        }
    }
    CHECK(effort);
    CHECK(cursor);
    REQUIRE(block_on(exec, (*session)->shutdown()));
}
