#include <doctest/doctest.h>

#include <algorithm>

#include "loa/harness/claude/driver.hpp"
#include "loa/harness/claude/mapper.hpp"
#include <memory>

#include "loa/core/sink.hpp"
#include "loa/core/writer.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/runtime/tempfile.hpp"
#include "../kit/support.hpp"
#include <asio/steady_timer.hpp>
#include <asio/this_coro.hpp>

namespace {
struct Recorder final : loa::contracts::HostTools {
    loa::contracts::Awaitable<std::expected<std::string, std::string>> call(
        std::string session_id, std::string tool, std::string args) const override {
        CHECK(session_id == "app-1");
        CHECK(tool == "project_search");
        CHECK(args == R"({"query":"auth"})");
        co_return std::string("3 hits in auth.rs");
    }
};

loa::contracts::Awaitable<std::optional<loa::core::NodeId>> waiting_decision(
    const std::shared_ptr<loa::core::MemorySink>& sink) {
    asio::steady_timer timer(co_await asio::this_coro::executor);
    for (int i = 0; i < 500; ++i) {
        auto found = sink->with_dom([](const loa::core::Dom& dom) -> std::optional<loa::core::NodeId> {
            for (const auto& node : dom.iter())
                if (node.kind == loa::core::ElementKind::Decision &&
                    node.str_attr(loa::core::attr::STATUS) == "waiting") return node.id;
            return std::nullopt;
        });
        if (found) co_return found;
        timer.expires_after(std::chrono::milliseconds(10));
        co_await timer.async_wait(asio::use_awaitable);
    }
    co_return std::nullopt;
}
}
#include "loa/runtime/json.hpp"

using namespace loa::harness::claude;
using loa::runtime::json::Value;
using loa::runtime::json::Object;
namespace json = loa::runtime::json;

TEST_CASE("claude: command arguments retain Rust flag order and MCP JSON") {
    loa::contracts::SessionConfig config;
    config.model = "sonnet";
    config.effort = "high";
    config.mode = loa::core::SessionMode::Plan;
    config.context = "project context";
    config.disabled_tools = {"WebFetch", "WebSearch"};
    config.mcp_servers.push_back({"browser", loa::contracts::McpStdio{"browser-mcp", {"--headless"}, {{"TOKEN", "a"}}}});
    config.mcp_servers.push_back({"remote", loa::contracts::McpHttp{"https://example.test/mcp", {{"Authorization", "Bearer a"}}}});
    config.host_tools.push_back({"search", "Search", "{}"});
    auto args = command_args(config, {"--resume=session"});
    const std::vector<std::string> prefix{"--print", "--output-format", "stream-json", "--verbose", "--input-format", "stream-json",
        "--forward-subagent-text", "--include-partial-messages", "--permission-prompt-tool", "stdio", "--model", "sonnet",
        "--effort", "high", "--permission-mode", "plan", "--append-system-prompt", "project context",
        "--disallowedTools", "WebFetch,WebSearch", "--mcp-config"};
    REQUIRE(args.size() == prefix.size() + 2);
    CHECK(std::equal(prefix.begin(), prefix.end(), args.begin()));
    CHECK(args.back() == "--resume=session");
    auto mcp = json::parse(args[args.size() - 2]);
    REQUIRE(mcp);
    auto* servers = mcp->find("mcpServers");
    REQUIRE(servers);
    CHECK(*servers->find("loa-host")->find("type") == Value("sdk"));
    CHECK(*servers->find("browser")->find("command") == Value("browser-mcp"));
    CHECK(*servers->find("remote")->find("url") == Value("https://example.test/mcp"));
    CHECK(args[args.size() - 2] == R"({"mcpServers":{"browser":{"args":["--headless"],"command":"browser-mcp","env":{"TOKEN":"a"},"type":"stdio"},"loa-host":{"name":"loa-host","type":"sdk"},"remote":{"headers":{"Authorization":"Bearer a"},"type":"http","url":"https://example.test/mcp"}}})");
}

TEST_CASE("claude: control framing sends and classifies stream JSON lines") {
    ClaudeControl framing;
    auto request = framing.request(Value("loa_1"), "initialize", Value(Object{{"hooks", nullptr}}));
    CHECK(json::dump(request) == R"({"request":{"hooks":null,"subtype":"initialize"},"request_id":"loa_1","type":"control_request"})");
    auto response = framing.classify(Value(Object{{"type", "control_response"}, {"response", Object{{"request_id", "loa_1"}, {"subtype", "success"}, {"response", Object{{"models", loa::runtime::json::Array{}}}}}}}));
    REQUIRE(response);
    auto* answer = std::get_if<loa::harness::kit::incoming::Response>(&*response);
    REQUIRE(answer);
    CHECK(answer->id == "loa_1");
    CHECK(answer->result.has_value());
    CHECK(answer->result->find("models") != nullptr);
    auto event = framing.classify(Value(Object{{"type", "system"}, {"subtype", "init"}}));
    REQUIRE(event);
    CHECK(std::get<loa::harness::kit::incoming::Notification>(*event).method == "system");
    auto failure = framing.classify(Value(Object{{"type", "control_response"}, {"response", Object{{"request_id", "loa_2"}, {"subtype", "error"}, {"error", "bad model"}}}}));
    REQUIRE(failure);
    CHECK(std::get<loa::harness::kit::incoming::Response>(*failure).result.error().message().find("bad model") != std::string::npos);
}

TEST_CASE("claude: fake CLI initializes and handles an interrupted turn") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto script = dir->path() / "claude";
    REQUIRE(loa::runtime::exec::write_executable(script, loa::harness::test::read_file(LOA_HARNESS_FIXTURES "/claude/fake-cli.sh")));
    loa::contracts::SessionConfig config;
    config.cwd = dir->path();
    config.executable = script;
    config.session_id = "app-1";
    config.node = "node-1";
    config.env["ARGS_FILE"] = (dir->path() / "args").string();
    config.env["LOG_FILE"] = (dir->path() / "stdin").string();
    auto sink = std::make_shared<loa::core::MemorySink>();
    loa::runtime::Executor executor(2);
    ClaudeAdapter adapter;
    auto session = loa::harness::test::block_on(executor, adapter.start(config, sink));
    REQUIRE(session);
    auto models = loa::harness::test::block_on(executor, (*session)->models());
    REQUIRE(models);
    REQUIRE(models->size() == 2);
    CHECK((*models)[1].id == "haiku");
    CHECK((*models)[1].efforts == std::vector<std::string>{"low", "medium", "high"});
    REQUIRE(loa::harness::test::block_on(executor, (*session)->send({"hang please", {}, "alice"})));
    REQUIRE(loa::harness::test::block_on(executor, (*session)->interrupt()));
    CHECK((*session)->state() == loa::contracts::SessionState::Idle);
    REQUIRE(loa::harness::test::block_on(executor, (*session)->set_model({"other-model", std::nullopt})));
    auto rejected = loa::harness::test::block_on(executor, (*session)->set_model({"bad-model", std::nullopt}));
    CHECK_FALSE(rejected);
    REQUIRE(loa::harness::test::block_on(executor, (*session)->set_mode(loa::core::SessionMode::Plan)));
    REQUIRE(loa::harness::test::block_on(executor, (*session)->shutdown()));
    CHECK((*session)->state() == loa::contracts::SessionState::Closed);
    auto lines = loa::harness::test::read_file(dir->path() / "stdin");
    CHECK(lines.find("\"subtype\":\"initialize\"") != std::string::npos);
    CHECK(lines.find("\"subtype\":\"interrupt\"") != std::string::npos);
    CHECK(lines.find("\"subtype\":\"set_permission_mode\"") != std::string::npos);
    CHECK(loa::harness::test::read_file(dir->path() / "args").find("--permission-prompt-tool\nstdio\n") != std::string::npos);
}

TEST_CASE("claude: fake CLI approval and host MCP tool complete a turn") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto script = dir->path() / "claude";
    REQUIRE(loa::runtime::exec::write_executable(script, loa::harness::test::read_file(LOA_HARNESS_FIXTURES "/claude/fake-cli.sh")));
    loa::contracts::SessionConfig config;
    config.cwd = dir->path();
    config.executable = script;
    config.session_id = "app-1";
    config.node = "node-1";
    config.env["ARGS_FILE"] = (dir->path() / "args").string();
    config.env["LOG_FILE"] = (dir->path() / "stdin").string();
    config.host_tools.push_back({"project_search", "Search project sessions", R"({"type":"object"})"});
    config.host_tool_handler = std::make_shared<Recorder>();
    auto sink = std::make_shared<loa::core::MemorySink>();
    loa::runtime::Executor executor(2);
    ClaudeAdapter adapter;
    auto session = loa::harness::test::block_on(executor, adapter.start(config, sink));
    REQUIRE(session);
    REQUIRE(loa::harness::test::block_on(executor, (*session)->send({"please use the tool", {}, "alice"})));
    auto decision = loa::harness::test::block_on(executor, waiting_decision(sink));
    REQUIRE(decision);
    CHECK((*session)->state() == loa::contracts::SessionState::Waiting);
    REQUIRE(loa::harness::test::block_on(executor, (*session)->answer(*decision, {{"allow_once"}, std::nullopt, "alice"})));
    REQUIRE(loa::harness::test::block_on(executor, (*session)->wait_idle()));
    CHECK((*session)->state() == loa::contracts::SessionState::Idle);
    auto dom = sink->dom();
    CHECK(dom.root()->str_attr(loa::core::attr::NATIVE_ID) == "cc-fake");
    auto turns = dom.children_of_kind(dom.root()->id, loa::core::ElementKind::Turn);
    REQUIRE(turns.size() == 1);
    CHECK(turns.front()->str_attr(loa::core::attr::STATUS) == "done");
    CHECK(loa::harness::test::read_file(dir->path() / "stdin").find("\"mcp_response\"") != std::string::npos);
    REQUIRE(loa::harness::test::block_on(executor, (*session)->shutdown()));
}

TEST_CASE("claude: resume_args and fork_point follow the latest transcript line of a cut DOM") {
    using namespace loa::core;
    namespace claude = loa::harness::claude;
    MemorySink sink;
    DomWriter w(sink);
    auto root = w.insert(NodeId::DOCUMENT, ElementKind::Session,
        {{std::string(attr::ID), "s"}, {std::string(attr::HARNESS), "claude-code"}, {std::string(attr::NODE), ""},
         {std::string(attr::CREATED), 0}, {std::string(attr::SCHEMA), 1}}).value();
    CHECK_FALSE(claude::resume_args(sink.dom(), false).has_value());
    REQUIRE(w.set(root, attr::NATIVE_ID, "sess-1"));
    CHECK(claude::resume_args(sink.dom(), false).value() == std::vector<std::string>{"--resume=sess-1"});
    CHECK(claude::resume_args(sink.dom(), true).value() == std::vector<std::string>{"--resume=sess-1", "--fork-session"});
    auto turn = w.insert(root, ElementKind::Turn, {{std::string(attr::STATUS), "done"}}).value();
    REQUIRE(w.insert(turn, ElementKind::Message, {{std::string(attr::ROLE), "user"}, {std::string(attr::ID), "u-1"}}, "hi"));
    auto call = w.insert(turn, ElementKind::Call, {{std::string(attr::TOOL), "Read"}, {std::string(claude::UUID_ATTR), "u-2"}}).value();
    CHECK(claude::fork_point(sink.dom()) == "u-2");
    REQUIRE(w.insert(call, ElementKind::Result, {{std::string(claude::UUID_ATTR), "u-3"}}, "out"));
    CHECK(claude::fork_point(sink.dom()) == "u-3");
    CHECK(claude::resume_args(sink.dom(), true).value() ==
          std::vector<std::string>{"--resume=sess-1", "--fork-session", "--resume-session-at=u-3"});
}
