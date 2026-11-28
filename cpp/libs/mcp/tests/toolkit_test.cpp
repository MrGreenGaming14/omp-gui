// `ToolSetMcp`: a two-tool `ToolSet` served over stdio (pipes) and streamable HTTP.

#include <doctest/doctest.h>

#include <asio.hpp>
#include <httplib.h>
#include <unistd.h>

#include <chrono>
#include <future>
#include <thread>

#include "loa/mcp/host_tools.hpp"
#include "loa/mcp/toolkit.hpp"
#include "loa/runtime/executor.hpp"

using namespace loa::mcp;
namespace json = loa::runtime::json;
namespace contracts = loa::contracts;
using contracts::Awaitable;
using contracts::ToolResult;

namespace {

struct Echo final : contracts::DynTool {
    contracts::HostToolSpec spec() const override {
        return {"echo", "Echoes `text`", R"({"type":"object","properties":{"text":{"type":"string"}}})"};
    }
    contracts::ToolAnnotations annotations() const override { return {"Echo", true, false}; }
    Awaitable<ToolResult> call(const contracts::CallCtx& ctx, json::Value args) const override {
        const auto* text = args.find("text");
        std::string out = text && text->as_string() ? *text->as_string() : std::string();
        if (!ctx.session_id.empty()) out += " from " + ctx.session_id;
        co_return contracts::ToolOutput::text(out);
    }
};

// Waits a long time unless cancelled.
struct Slow final : contracts::DynTool {
    contracts::HostToolSpec spec() const override { return {"slow", "Sleeps", R"({"type":"object"})"}; }
    Awaitable<ToolResult> call(const contracts::CallCtx&, json::Value) const override {
        asio::steady_timer timer(co_await asio::this_coro::executor, std::chrono::seconds(30));
        co_await timer.async_wait(asio::use_awaitable);
        co_return contracts::ToolOutput::text("woke");
    }
};

ToolSetMcp server() {
    contracts::ToolSet tools;
    tools.add(std::make_shared<Echo>()).add(std::make_shared<Slow>());
    return ToolSetMcp(std::move(tools), "toolkit-test").with_instructions("Use echo.");
}

json::Value request(std::int64_t id, std::string_view method, json::Value params) {
    return json::Object{{"jsonrpc", "2.0"}, {"id", id}, {"method", method}, {"params", std::move(params)}};
}

struct Pipes {
    int in[2];
    int out[2];
    Pipes() {
        REQUIRE(::pipe(in) == 0);
        REQUIRE(::pipe(out) == 0);
    }
    void send(const json::Value& message) const {
        const std::string line = json::dump(message) + "\n";
        REQUIRE(::write(in[1], line.data(), line.size()) == static_cast<ssize_t>(line.size()));
    }
    json::Value receive() {
        for (;;) {
            if (auto nl = buffer.find('\n'); nl != std::string::npos) {
                auto parsed = json::parse(std::string_view(buffer).substr(0, nl));
                buffer.erase(0, nl + 1);
                REQUIRE(parsed);
                return *parsed;
            }
            char chunk[4096];
            const auto n = ::read(out[0], chunk, sizeof chunk);
            REQUIRE(n > 0);
            buffer.append(chunk, static_cast<std::size_t>(n));
        }
    }
    std::string buffer;
};

const json::Value& at(const json::Value& value, std::string_view key) {
    const auto* found = value.find(key);
    REQUIRE(found);
    return *found;
}

} // namespace

TEST_CASE("toolkit: a two-tool ToolSet over stdio") {
    loa::runtime::Executor exec(2);
    Pipes pipes;
    auto mcp = server();
    std::promise<Result<void>> done;
    std::thread serving([&] { done.set_value(mcp.serve(exec, pipes.in[0], pipes.out[1])); });

    pipes.send(request(1, "initialize", json::Object{{"protocolVersion", "2025-03-26"}, {"capabilities", json::Object{}},
                                                     {"clientInfo", json::Object{{"name", "t"}, {"version", "1"}}}}));
    auto init = pipes.receive();
    CHECK(json::dump(at(init, "result")) ==
          R"({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"toolkit-test","version":"0.1.0"},"instructions":"Use echo."})");
    pipes.send(json::Object{{"jsonrpc", "2.0"}, {"method", "notifications/initialized"}});

    pipes.send(request(2, "tools/list", json::Object{}));
    auto list = pipes.receive();
    CHECK(json::dump(at(at(list, "result"), "tools")) ==
          R"([{"name":"echo","title":"Echo","description":"Echoes `text`","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}},"annotations":{"readOnlyHint":true,"destructiveHint":false}},)"
          R"({"name":"slow","description":"Sleeps","inputSchema":{"type":"object"}}])");

    pipes.send(request(3, "tools/call", json::Object{{"name", "echo"}, {"arguments", json::Object{{"text", "hi"}}}}));
    auto echoed = pipes.receive();
    CHECK(json::dump(at(echoed, "result")) == R"({"content":[{"type":"text","text":"hi"}],"isError":false})");

    pipes.send(request(4, "tools/call", json::Object{{"name", "nope"}}));
    auto unknown = pipes.receive();
    CHECK(*at(at(unknown, "error"), "code").as_i64() == -32602);
    CHECK(*at(at(unknown, "error"), "message").as_string() == "unknown tool: nope");

    // A cancelled call is answered at once and its coroutine is cancelled.
    pipes.send(request(5, "tools/call", json::Object{{"name", "slow"}}));
    pipes.send(json::Object{{"jsonrpc", "2.0"}, {"method", "notifications/cancelled"}, {"params", json::Object{{"requestId", 5}}}});
    auto cancelled = pipes.receive();
    CHECK(*at(cancelled, "id").as_i64() == 5);
    CHECK(json::dump(at(cancelled, "result")) == R"({"content":[{"type":"text","text":"tool call cancelled"}],"isError":true})");

    ::close(pipes.in[1]);
    auto finished = done.get_future();
    REQUIRE(finished.wait_for(std::chrono::seconds(10)) == std::future_status::ready);
    CHECK(finished.get());
    serving.join();
    ::close(pipes.in[0]);
    ::close(pipes.out[0]);
    ::close(pipes.out[1]);
}

TEST_CASE("toolkit: streamable HTTP admits callers and names them") {
    loa::runtime::Executor exec(2);
    auto served = server().serve_http(exec, "127.0.0.1", 0, [](const HttpHeaders& headers) -> std::optional<contracts::CallCtx> {
        for (const auto& [key, value] : headers) {
            if (key == "Authorization" && value.starts_with("Bearer ")) return contracts::CallCtx{value.substr(7)};
        }
        return std::nullopt;
    });
    REQUIRE(served);
    CHECK(served->url() == "http://127.0.0.1:" + std::to_string(served->port()) + "/mcp");
    httplib::Client client("127.0.0.1", served->port());
    const std::string init = json::dump(request(1, "initialize", json::Object{{"protocolVersion", "2025-06-18"}}));
    auto refused = client.Post("/mcp", init, "application/json");
    REQUIRE(refused);
    CHECK(refused->status == 401);

    httplib::Headers auth = {{"Authorization", "Bearer s-1"}, {"Accept", "application/json, text/event-stream"}};
    auto opened = client.Post("/mcp", auth, init, "application/json");
    REQUIRE(opened);
    CHECK(opened->status == 200);
    CHECK_FALSE(opened->get_header_value("Mcp-Session-Id").empty());
    auto note = client.Post("/mcp", auth, R"({"jsonrpc":"2.0","method":"notifications/initialized"})", "application/json");
    REQUIRE(note);
    CHECK(note->status == 202);
    auto call = client.Post("/mcp", auth, json::dump(request(2, "tools/call", json::Object{{"name", "echo"}, {"arguments", json::Object{{"text", "hi"}}}})),
                            "application/json");
    REQUIRE(call);
    auto body = json::parse(call->body);
    REQUIRE(body);
    CHECK(json::dump(at(*body, "result")) == R"({"content":[{"type":"text","text":"hi from s-1"}],"isError":false})");
    auto get = client.Get("/mcp", auth);
    REQUIRE(get);
    CHECK(get->status == 405);
}

TEST_CASE("toolkit: results round-trip through the MCP wire form") {
    contracts::ToolOutput output;
    output.blocks.emplace_back(contracts::TextBlock{"stale tag"});
    output.blocks.push_back(*image("aGk=", "image/png"));
    output.is_error = true;
    auto wire = to_result(output);
    CHECK(json::dump(wire) ==
          R"({"content":[{"type":"text","text":"stale tag"},{"type":"image","data":"aGk=","mimeType":"image/png"}],"isError":true})");
    auto back = model::CallToolResult::from_json(wire);
    REQUIRE(back);
    CHECK(host_tools::to_output(*back) == output);
    CHECK_FALSE(image("not base64!", "image/png"));
}
