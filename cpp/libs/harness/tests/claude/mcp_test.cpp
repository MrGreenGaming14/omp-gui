#include <doctest/doctest.h>

#include <expected>
#include <memory>
#include <string>
#include <vector>

#include "../kit/support.hpp"
#include "loa/harness/claude/mcp.hpp"

namespace mcp = loa::harness::claude::mcp;
namespace json = loa::runtime::json;
using loa::contracts::HostToolSpec;
using loa::contracts::HostTools;
using loa::harness::kit::Json;

namespace {

Json parse(std::string_view text) { return json::parse(text).value(); }

class RecordingTools final : public HostTools {
  public:
    mutable std::vector<std::string> calls;
    std::expected<std::string, std::string> outcome = "done";

    loa::contracts::Awaitable<std::expected<std::string, std::string>> call(
        std::string session, std::string tool, std::string args) const override {
        calls.push_back(std::move(session) + "|" + std::move(tool) + "|" + std::move(args));
        co_return outcome;
    }
};

Json invoke(loa::runtime::Executor& exec, const std::vector<HostToolSpec>& specs,
            const std::shared_ptr<const HostTools>& handler, const Json& request) {
    return loa::harness::test::block_on(exec, mcp::handle(specs, handler, "session-42", request));
}

} // namespace

TEST_CASE("claude MCP initialize, ping and notifications acknowledge with JSON-RPC envelopes") {
    loa::runtime::Executor exec(1);
    const std::vector<HostToolSpec> specs;
    const std::shared_ptr<const HostTools> handler;
    CHECK(mcp::HOST_SERVER == "loa-host");
    const Json initialized = invoke(exec, specs, handler, parse(R"({"method":"initialize","id":"a","params":{"protocolVersion":"2024-11-05"}})"));
    CHECK(initialized == parse(R"({"jsonrpc":"2.0","id":"a","result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"loa-host","version":"0.1.0"}}})"));
    const Json fallback = invoke(exec, specs, handler, parse(R"({"method":"initialize","id":1,"params":{"protocolVersion":12}})"));
    CHECK(*fallback.find("result")->find("protocolVersion") == Json("2025-06-18"));
    const Json empty = invoke(exec, specs, handler, parse(R"({"method":"initialize","params":{"protocolVersion":""}})"));
    CHECK(*empty.find("result")->find("protocolVersion") == Json(""));
    CHECK(invoke(exec, specs, handler, parse(R"({"method":"ping","id":null})")) == parse(R"({"jsonrpc":"2.0","id":null,"result":{}})"));
    CHECK(invoke(exec, specs, handler, parse(R"({"method":"notifications/initialized"})")) == parse(R"({"jsonrpc":"2.0","id":null,"result":{}})"));
    CHECK(invoke(exec, specs, handler, parse(R"({"method":"notifications/other","id":false})")) == parse(R"({"jsonrpc":"2.0","id":false,"result":{}})"));
}

TEST_CASE("claude MCP lists host tools with object schemas and malformed-schema fallback") {
    loa::runtime::Executor exec(1);
    const std::vector<HostToolSpec> specs{
        {"search", "Search project", R"({"type":"object","properties":{"query":{"type":"string"}}})"},
        {"bad", "invalid", "{"},
        {"array", "not an object", "[]"},
        {"scalar", "not an object", "null"},
    };
    const Json listed = invoke(exec, specs, {}, parse(R"({"method":"tools/list","id":42})"));
    CHECK(listed == parse(R"({"jsonrpc":"2.0","id":42,"result":{"tools":[{"name":"search","description":"Search project","inputSchema":{"type":"object","properties":{"query":{"type":"string"}}}},{"name":"bad","description":"invalid","inputSchema":{"type":"object"}},{"name":"array","description":"not an object","inputSchema":{"type":"object"}},{"name":"scalar","description":"not an object","inputSchema":{"type":"object"}}]}})"));
    CHECK(invoke(exec, {}, {}, parse(R"({"method":"tools/list"})")) == parse(R"({"jsonrpc":"2.0","id":null,"result":{"tools":[]}})"));
}

TEST_CASE("claude MCP calls only registered host tools, forwards serialized arguments and session id") {
    loa::runtime::Executor exec(1);
    const std::vector<HostToolSpec> specs{{"search", "Search", "{}"}};
    auto handler = std::make_shared<RecordingTools>();
    CHECK(invoke(exec, specs, handler, parse(R"({"method":"tools/call","id":"c","params":{"name":"search","arguments":{"query":"café","limit":2}}})")) ==
          parse(R"({"jsonrpc":"2.0","id":"c","result":{"content":[{"type":"text","text":"done"}],"isError":false}})"));
    REQUIRE(handler->calls.size() == 1);
    CHECK(handler->calls[0] == "session-42|search|{\"query\":\"café\",\"limit\":2}");
    invoke(exec, specs, handler, parse(R"({"method":"tools/call","params":{"name":"search"}})"));
    invoke(exec, specs, handler, parse(R"({"method":"tools/call","params":{"name":"search","arguments":null}})"));
    REQUIRE(handler->calls.size() == 3);
    CHECK(handler->calls[1] == "session-42|search|{}");
    CHECK(handler->calls[2] == "session-42|search|null");

    handler->outcome = std::unexpected("failed");
    CHECK(invoke(exec, specs, handler, parse(R"({"method":"tools/call","id":9,"params":{"name":"search","arguments":[1,true]}})")) ==
          parse(R"({"jsonrpc":"2.0","id":9,"result":{"content":[{"type":"text","text":"failed"}],"isError":true}})"));
    REQUIRE(handler->calls.size() == 4);
    CHECK(handler->calls[3] == "session-42|search|[1,true]");

    CHECK(invoke(exec, specs, handler, parse(R"({"method":"tools/call","params":{"name":"other"}})")) ==
          parse(R"({"jsonrpc":"2.0","id":null,"result":{"content":[{"type":"text","text":"unknown tool `other`"}],"isError":true}})"));
    CHECK(handler->calls.size() == 4);
    CHECK(invoke(exec, specs, {}, parse(R"({"method":"tools/call","params":{"name":"search"}})")) ==
          parse(R"({"jsonrpc":"2.0","id":null,"result":{"content":[{"type":"text","text":"host tools have no handler"}],"isError":true}})"));
    CHECK(invoke(exec, specs, {}, parse(R"({"method":"tools/call"})")) ==
          parse(R"({"jsonrpc":"2.0","id":null,"result":{"content":[{"type":"text","text":"unknown tool ``"}],"isError":true}})"));
}

TEST_CASE("claude MCP unknown methods return -32601 with original id") {
    loa::runtime::Executor exec(1);
    CHECK(invoke(exec, {}, {}, parse(R"({"method":"tools/other","id":{"nested":1}})")) ==
          parse(R"({"jsonrpc":"2.0","id":{"nested":1},"error":{"code":-32601,"message":"method `tools/other` not found"}})"));
    CHECK(invoke(exec, {}, {}, parse(R"({"method":7})")) ==
          parse(R"({"jsonrpc":"2.0","id":null,"error":{"code":-32601,"message":"method `` not found"}})"));
    CHECK(mcp::error(parse(R"({"id":"x"})"), "bad request") ==
          parse(R"({"jsonrpc":"2.0","id":"x","error":{"code":-32601,"message":"bad request"}})"));
}
