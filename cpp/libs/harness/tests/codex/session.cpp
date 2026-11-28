#include <doctest/doctest.h>

#include <filesystem>
#include <fstream>
#include <string>

#include "../kit/support.hpp"
#include "loa/harness/codex/adapter.hpp"
#include "loa/harness/codex/mapper.hpp"
#include "loa/harness/codex/session.hpp"
#include "loa/runtime/tempfile.hpp"

namespace codex = loa::harness::codex;
namespace json = loa::runtime::json;
namespace contracts = loa::contracts;
using loa::harness::test::block_on;

TEST_CASE("codex: MCP overrides quote values and sanitize config keys") {
    contracts::SessionConfig config;
    config.mcp_servers = {
        {"stdio.server", contracts::McpStdio{"/bin/tool", {"with space", "quote\""}, {{"X", "a\nb"}}}},
        {"remote/name", contracts::McpHttp{"https://example.test/mcp", {{"Authorization", "Bearer a"}}}},
    };
    CHECK(codex::mcp_overrides(config) == std::vector<std::string>{
        "-c", "mcp_servers.stdio_server.command=\"/bin/tool\"",
        "-c", "mcp_servers.stdio_server.args=[\"with space\",\"quote\\\"\"]",
        "-c", "mcp_servers.stdio_server.env={\"X\"=\"a\\nb\"}",
        "-c", "mcp_servers.remote_name.url=\"https://example.test/mcp\"",
        "-c", "mcp_servers.remote_name.http_headers={\"Authorization\"=\"Bearer a\"}",
    });
}

TEST_CASE("codex: thread requests retain owned fork ids and start-only tools") {
    contracts::SessionConfig config;
    config.cwd = "/tmp/work";
    config.model = "model-a";
    config.context = "instructions";
    config.host_tools = {{"search", "Search", R"({"type":"object"})"}};
    auto [start_method, started] = codex::open_request(config, codex::Open::start());
    CHECK(start_method == "thread/start");
    CHECK(json::dump(started) == R"({"cwd":"/tmp/work","model":"model-a","developerInstructions":"instructions","approvalPolicy":"on-request","sandbox":"workspace-write","dynamicTools":[{"type":"function","name":"search","description":"Search","inputSchema":{"type":"object"}}]})");
    auto [resume_method, resumed] = codex::open_request(config, codex::Open::resume(std::string("thread-1")));
    CHECK(resume_method == "thread/resume");
    CHECK(json::dump(resumed) == R"({"cwd":"/tmp/work","model":"model-a","developerInstructions":"instructions","threadId":"thread-1","excludeTurns":true})");
    auto fork = codex::Open::fork(std::string("thread-2"), std::string("turn-2"));
    auto [fork_method, forked] = codex::open_request(config, fork);
    CHECK(fork_method == "thread/fork");
    CHECK(json::dump(forked) == R"({"cwd":"/tmp/work","model":"model-a","developerInstructions":"instructions","threadId":"thread-2","excludeTurns":true,"lastTurnId":"turn-2"})");
}

TEST_CASE("codex: turn input, effort, and collaboration mode") {
    contracts::Prompt prompt{"hello", {{"/tmp/shot.png", std::nullopt}, {"/tmp/notes.txt", std::nullopt}}, "user"};
    auto params = codex::turn_params(prompt, "thread-1", "model-a", "high", loa::core::SessionMode::Plan);
    CHECK(json::dump(params) == R"({"threadId":"thread-1","input":[{"type":"text","text":"hello","text_elements":[]},{"type":"localImage","path":"/tmp/shot.png"},{"type":"text","text":"Attached file: /tmp/notes.txt","text_elements":[]}],"model":"model-a","effort":"high","collaborationMode":{"mode":"plan","settings":{"model":"model-a","reasoning_effort":"high","developer_instructions":null}}})");
    auto no_model = codex::turn_params(prompt, "thread-1", std::nullopt, std::nullopt, loa::core::SessionMode::Plan);
    CHECK(no_model.find("collaborationMode") == nullptr);
}

TEST_CASE("codex: session opens, defers early requests, sends turns and closes stdin") {
    loa::runtime::Executor executor(2);
    auto dir = loa::runtime::TempDir::create().value();
    const std::string script = R"(read -r init
printf '%s\n' '{"id":"early","method":"unrecognized","params":{}}'
read -r early
printf '%s\n' "$init" "$early" > "$OUT/requests"
printf '%s\n' '{"id":1,"result":{}}'
read -r initialized
printf '%s\n' "$initialized" >> "$OUT/requests"
read -r open
printf '%s\n' "$open" >> "$OUT/requests"
printf '%s\n' '{"id":2,"result":{"thread":{"id":"thread-1"},"model":"model-a"}}'
read -r turn
printf '%s\n' "$turn" >> "$OUT/requests"
printf '%s\n' '{"id":3,"result":{"turn":{"id":"turn-1"}}}'
read -r interrupt
printf '%s\n' "$interrupt" >> "$OUT/requests"
printf '%s\n' '{"id":4,"result":{}}'
read -r models
printf '%s\n' "$models" >> "$OUT/requests"
printf '%s\n' '{"id":5,"result":{"data":[{"model":"model-a","displayName":"Model A","supportedReasoningEfforts":[{"reasoningEffort":"low"},"high"]},{"model":"hidden","hidden":true}],"nextCursor":null}}'
read -r compact
printf '%s\n' "$compact" >> "$OUT/requests"
printf '%s\n' '{"id":6,"result":{}}'
while read -r line; do printf '%s\n' "$line" >> "$OUT/requests"; done)";
    codex::CodexAdapter adapter("test");
    adapter = adapter.with_executable("/bin/bash").with_args({"-c", script});
    contracts::SessionConfig config;
    config.session_id = "session-1";
    config.node = "test";
    config.cwd = dir.path();
    config.env["OUT"] = dir.path().string();
    config.effort = "high";
    auto sink = std::make_shared<loa::core::MemorySink>();
    auto result = block_on(executor, codex::CodexSession::open(
        adapter, config, sink, codex::Open::start(),
        [config](std::string_view thread, std::optional<std::string_view> model, loa::core::DomWriter& w) {
            return codex::Mapper::start(config, thread, model, w);
        }));
    REQUIRE(result);
    auto& session = **result;
    CHECK(session.state() == contracts::SessionState::Idle);
    CHECK(block_on(executor, session.send(contracts::Prompt{"hello", {}, "user"})));
    CHECK(block_on(executor, session.interrupt()));
    auto models = block_on(executor, session.models());
    REQUIRE(models);
    CHECK(*models == std::vector<contracts::HarnessModel>{{"model-a", "Model A", "openai", {"low", "high"}}});
    CHECK(block_on(executor, session.compact(std::nullopt)));
    CHECK(block_on(executor, session.shutdown()));
    CHECK(session.state() == contracts::SessionState::Closed);
    std::ifstream in(dir.path() / "requests");
    std::vector<json::Value> frames;
    for (std::string line; std::getline(in, line); ) {
        auto parsed = json::parse(line);
        REQUIRE(parsed);
        frames.push_back(std::move(*parsed));
    }
    REQUIRE(frames.size() >= 8);
    CHECK(json::dump(frames[1]) == R"({"id":"early","error":{"code":-32601,"message":"not ready"}})");
    CHECK(json::dump(frames[0]).find("\"method\":\"initialize\"") != std::string::npos);
    CHECK(json::dump(frames[3]).find("\"method\":\"thread/start\"") != std::string::npos);
    CHECK(json::dump(frames[4]).find("\"method\":\"turn/start\"") != std::string::npos);
    CHECK(json::dump(frames[5]).find("\"method\":\"turn/interrupt\"") != std::string::npos);
    CHECK(json::dump(frames[6]).find("\"method\":\"model/list\"") != std::string::npos);
    CHECK(json::dump(frames[7]).find("\"method\":\"thread/compact/start\"") != std::string::npos);
}
