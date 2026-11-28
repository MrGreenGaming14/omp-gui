// The omp tools bridge: argument parsing, omp frames, the scripted provider (against goldens
// written by the Rust code) and one full call against the omp RPC test double.

#include <doctest/doctest.h>

#include <asio.hpp>
#include <httplib.h>

#include <filesystem>
#include <fstream>
#include <iterator>

#include "loa/harness/testing/testing.hpp"
#include "loa/mcp/bridge.hpp"
#include "loa/mcp/provider.hpp"
#include "loa/mcp/toolkit.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::mcp;
using namespace loa::mcp::bridge;
namespace json = loa::runtime::json;
namespace contracts = loa::contracts;
namespace fs = std::filesystem;

namespace {

std::string read_file(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    REQUIRE(in);
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}

fs::path golden(std::string_view name) { return fs::path(LOA_GOLDEN_DIR) / "mcp" / "bridge" / name; }

std::expected<OmpBridgeConfig, std::string> parse(std::vector<std::string> args) { return OmpBridgeConfig::parse(args); }

template <typename T>
T block_on(loa::runtime::Executor& exec, asio::awaitable<T> aw) {
    return asio::co_spawn(exec.context(), std::move(aw), asio::use_future).get();
}

} // namespace

TEST_CASE("bridge: args round trip and the spec runs the app subcommand") {
    auto config = OmpBridgeConfig::make("/work", "/state");
    config.program = "/opt/omp";
    config.tools = {"read", "edit"};
    config.config_overlays = {"/c.yml"};
    const std::array<std::string_view, 1> prefix = {"omp-bridge"};
    auto spec = config.session_spec("/usr/bin/loa-app", prefix);
    CHECK(spec.name == "omp");
    const auto& stdio = std::get<contracts::McpStdio>(spec.transport);
    CHECK(stdio.command == "/usr/bin/loa-app");
    CHECK(stdio.args[0] == "omp-bridge");
    auto parsed = OmpBridgeConfig::parse(std::span(stdio.args).subspan(1));
    REQUIRE(parsed);
    CHECK(parsed->cwd == config.cwd);
    CHECK(parsed->state_dir == config.state_dir);
    CHECK(parsed->program == config.program);
    CHECK(parsed->tools == config.tools);
    CHECK(parsed->config_overlays == config.config_overlays);

    CHECK(parse({"--cwd"}).error() == "--cwd needs a value");
    CHECK(parse({"--bogus", "x"}).error() == "unknown argument `--bogus`");
    CHECK(parse({"--bogus"}).error() == "--bogus needs a value");
    CHECK(parse({"--cwd", "/w"}).error() == "--state-dir is required");
    CHECK(parse({}).error() == "--state-dir is required");
    auto defaults = parse({"--state-dir", "/s", "--config", "a.yml", "--config", "b.yml"});
    REQUIRE(defaults);
    CHECK(defaults->program == "omp");
    CHECK(defaults->cwd == fs::current_path());
    CHECK(defaults->tools == std::vector<std::string>{"read", "edit", "ast_grep", "ast_edit", "lsp", "debug"});
    CHECK(defaults->config_overlays == std::vector<fs::path>{"a.yml", "b.yml"});
    CHECK(parse({"--state-dir", "/s", "--tools", "a,,b"})->tools == std::vector<std::string>{"a", "", "b"});
    CHECK(USAGE == "usage: loa omp-bridge --state-dir DIR [--cwd DIR] [--omp PATH] [--tools a,b,...] [--config FILE]...");
}

TEST_CASE("bridge: schemas and results come from omp frames") {
    json::Array dumped = {
        *json::parse(R"({"name": "read", "description": "Read files", "parameters": {"type": "object"}})"),
        *json::parse(R"({"name": "edit", "description": "Hashline patches", "parameters": {"type": "object", "properties": {"input": {}}}})"),
    };
    const std::vector<std::string> wanted = {"edit", "read"};
    auto tools = select_tools(dumped, wanted);
    REQUIRE(tools);
    CHECK((*tools)[0].name == "edit");
    CHECK((*tools)[1].description == "Read files");
    CHECK((*tools)[0].input_schema.find("input") != std::string::npos);
    const std::vector<std::string> missing = {"lsp"};
    CHECK(select_tools(dumped, missing).error().to_string() == "omp bridge: unknown tool `lsp`");
    auto result = *json::parse(R"({"content": [{"type": "text", "text": "stale tag"}, {"type": "image", "data": "aGk=", "mimeType": "image/png"}, {"type": "other"}]})");
    auto out = tool_output(&result, true);
    CHECK(out.is_error);
    REQUIRE(out.blocks.size() == 3);
    CHECK(std::get<contracts::TextBlock>(out.blocks[0]).text == "stale tag");
    const auto& img = std::get<contracts::ImageBlock>(out.blocks[1]);
    CHECK(img.mime == "image/png");
    CHECK(img.data == std::vector<std::byte>{std::byte{'h'}, std::byte{'i'}});
    CHECK(std::get<contracts::TextBlock>(out.blocks[2]).text == R"({"type":"other"})");
    CHECK(tool_output(nullptr, false).blocks.empty());
}

TEST_CASE("bridge: only a marked user message of an agent request gets its tool call (Rust goldens)") {
    for (const auto* name : {"marked-parts", "marked-string", "tool-result", "plain-user", "no-tools"}) {
        CAPTURE(name);
        const auto body = read_file(golden(std::string("provider-") + name + ".request.json"));
        auto stream = completion(body);
        REQUIRE(stream);
        CHECK(*stream == read_file(golden(std::string("provider-") + name + ".sse")));
    }
    CHECK_FALSE(completion("not json"));
    json::Object args = {{"input", "[a.txt#B6C1]\n+BETA \"\xC3\xA9\" \xE2\x98\x83\n"}, {"limit", 3}, {"all", true}};
    CHECK(call_prompt("loa-omp-bridge-call-1", "edit", args) == read_file(golden("call-prompt.txt")));
    CHECK(models_config("http://127.0.0.1:40000/v1") == read_file(golden("models.yml")));
    CHECK(bridge_config() == read_file(golden("bridge-config.yml")));
}

TEST_CASE("bridge: the scripted provider serves only authorized chat completions") {
    auto provider = ScriptedProvider::start("tok");
    REQUIRE(provider);
    const std::string base = (*provider)->base_url();
    REQUIRE(base.starts_with("http://127.0.0.1:"));
    REQUIRE(base.ends_with("/v1"));
    const int port = std::stoi(base.substr(17, base.size() - 20));
    httplib::Client client("127.0.0.1", port);
    const std::string body = read_file(golden("provider-marked-string.request.json"));
    auto refused = client.Post("/v1/chat/completions", body, "application/json");
    REQUIRE(refused);
    CHECK(refused->status == 401);
    httplib::Headers auth = {{"Authorization", "Bearer tok"}};
    auto answered = client.Post("/v1/chat/completions", auth, body, "application/json");
    REQUIRE(answered);
    CHECK(answered->status == 200);
    CHECK(answered->get_header_value("Content-Type") == "text/event-stream");
    CHECK(answered->get_header_value("Cache-Control") == "no-cache");
    CHECK(answered->body == read_file(golden("provider-marked-string.sse")));
    auto bad = client.Post("/v1/chat/completions", auth, "{", "application/json");
    REQUIRE(bad);
    CHECK(bad->status == 400);
    auto elsewhere = client.Get("/v1/models", auth);
    REQUIRE(elsewhere);
    CHECK(elsewhere->status == 404);
}

TEST_CASE("bridge: an MCP tools/call runs through omp's prompt and returns tool_execution_end") {
    loa::runtime::Executor exec(3);
    auto dir = loa::runtime::TempDir::create().value();
    const std::string script = loa::harness::testing::handshake_v2() +
        "< get_state s\n"
        R"(> {"type":"response","id":"{{s}}","command":"get_state","success":true,"data":{"sessionId":"x","isStreaming":false,"dumpTools":[)"
        R"({"name":"read","description":"Read files","parameters":{"type":"object","properties":{"path":{"type":"string"}}}},)"
        R"({"name":"edit","description":"Edit","parameters":{"type":"object"}}]}})" "\n"
        "< prompt p\n"
        R"(> {"type":"response","id":"{{p}}","command":"prompt","success":true,"data":{"agentInvoked":true}})" "\n"
        R"(> {"type":"tool_execution_end","toolCallId":"loa-omp-bridge-call-1","toolName":"read","result":{"content":[{"type":"text","text":"hello"}]},"isError":false})" "\n"
        R"(> {"type":"prompt_result","id":"{{p}}","agentInvoked":true,"status":"completed"})" "\n";
    auto omp = loa::harness::testing::FakeProcess::install(dir.path(), script).value();
    auto config = OmpBridgeConfig::make(dir.path(), dir.path() / "state");
    config.program = omp.executable();
    config.tools = {"read"};
    {
        auto executor = block_on(exec, OmpExecutor::start(config));
        REQUIRE(executor);
        REQUIRE((*executor)->tools().size() == 1);
        auto mcp = ToolSetMcp((*executor)->tool_set(), SERVER_NAME).with_instructions(INSTRUCTIONS);
        json::Value call = json::Object{{"jsonrpc", "2.0"}, {"id", 7}, {"method", "tools/call"},
                                        {"params", json::Object{{"name", "read"}, {"arguments", json::Object{{"path", "a.txt"}}}}}};
        auto answer = block_on(exec, mcp.handle(std::move(call), {}));
        REQUIRE(answer);
        CHECK(json::dump(*answer) == R"({"jsonrpc":"2.0","id":7,"result":{"content":[{"type":"text","text":"hello"}],"isError":false}})");
        auto list = block_on(exec, mcp.handle(*json::parse(R"({"jsonrpc":"2.0","id":8,"method":"tools/list"})"), {}));
        REQUIRE(list);
        CHECK(json::dump(*list).find(R"("annotations")") == std::string::npos);
    }

    auto sent = omp.received();
    auto prompt = std::ranges::find_if(sent, [](const json::Value& frame) {
        const auto* type = frame.find("type");
        return type && type->as_string() && *type->as_string() == "prompt";
    });
    REQUIRE(prompt != sent.end());
    CHECK(*prompt->find("message")->as_string() == call_prompt("loa-omp-bridge-call-1", "read", json::Object{{"path", "a.txt"}}));
    const auto args = omp.args();
    const std::vector<std::string> expected = {"--mode", "rpc", "--no-ui", "--no-session", "--no-extensions", "--no-skills", "--no-rules",
                                               "--no-context-files", "--no-title", "--offline", "--auto-approve", "--model",
                                               "loa-omp-bridge/scripted", "--config", (dir.path() / "state" / "bridge-config.yml").string(),
                                               "--tools", "read"};
    CHECK(args == expected);
    CHECK(read_file(dir.path() / "state" / "bridge-config.yml") == bridge_config());
    CHECK(read_file(dir.path() / "state" / "agent" / "models.yml").find(R"("apiKey":"LOA_OMP_BRIDGE_TOKEN")") != std::string::npos);
}
