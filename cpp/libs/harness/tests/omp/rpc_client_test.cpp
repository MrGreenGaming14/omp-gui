#include <doctest/doctest.h>

#include <asio.hpp>

#include <future>
#include <string>

#include "loa/core/base64.hpp"
#include "loa/harness/omp/rpc/rpc.hpp"
#include "loa/harness/omp/rpc/typed.hpp"
#include "loa/harness/testing/testing.hpp"
#include "loa/runtime/executor.hpp"
#include "loa/runtime/tempfile.hpp"
#include "../kit/support.hpp"

using namespace loa::harness::omp::rpc;
namespace json = loa::runtime::json;
using loa::harness::test::block_on;
using loa::harness::testing::FakeProcess;
using loa::runtime::TempDir;

namespace {
struct Fake {
    TempDir dir;
    FakeProcess omp;
    SpawnOptions opts;
};

Fake fake(std::string_view script) {
    auto dir = TempDir::create().value();
    auto omp = FakeProcess::install(dir.path(), script).value();
    SpawnOptions opts(omp.executable());
    opts.ready_timeout = std::chrono::seconds(20);
    return {std::move(dir), std::move(omp), std::move(opts)};
}

std::string reply(std::string_view name, std::string_view command, std::string_view data) {
    return "> {\"type\":\"response\",\"id\":\"{{" + std::string(name) + "}}\",\"command\":\"" + std::string(command) +
           "\",\"success\":true,\"data\":" + std::string(data) + "}\n";
}

Incoming next(loa::runtime::Executor& exec, Events& events) {
    auto result = block_on(exec, events.recv());
    REQUIRE(result);
    return std::move(*result);
}

struct Echo final : HostHandler {
    Awaitable<HostToolOutput> tool_call(HostToolCall call) override {
        const auto* q = call.arguments.find("q");
        co_return HostToolOutput::ok(call.tool_name + ":" + (q && q->as_string() ? *q->as_string() : std::string{}));
    }
};
} // namespace

TEST_CASE("omp rpc: startup negotiates only v2, reports unsupported versions and early exit") {
    loa::runtime::Executor exec(3);
    {
        auto f = fake(loa::harness::testing::handshake_v2());
        auto spawned = block_on(exec, RpcClient::spawn(f.opts));
        REQUIRE(spawned);
        CHECK(spawned->client.protocol_version() == 2);
        CHECK(spawned->client.ready().supports_bash_exclude_from_context);
        auto status = block_on(exec, spawned->client.shutdown(std::chrono::seconds(10)));
        REQUIRE(status);
        auto exited = next(exec, spawned->events);
        CHECK(std::holds_alternative<incoming::Exited>(exited));
        auto sent = f.omp.received();
        REQUIRE(sent.size() == 1);
        CHECK(*sent[0].find("type")->as_string() == "negotiate_protocol");
        auto args = f.omp.args();
        REQUIRE(args.size() >= 2);
        CHECK(args[0] == "--mode");
        CHECK(args[1] == "rpc");
    }
    {
        auto f = fake("> {\"type\":\"ready\"}\n");
        auto spawned = block_on(exec, RpcClient::spawn(f.opts));
        REQUIRE(spawned);
        CHECK(spawned->client.protocol_version() == 1);
        REQUIRE(block_on(exec, spawned->client.shutdown(std::chrono::seconds(10))));
        CHECK(f.omp.received().empty());
    }
    {
        auto f = fake("> {\"type\":\"ready\",\"protocolVersion\":3,\"supportedProtocolVersions\":[3,4]}\n");
        auto spawned = block_on(exec, RpcClient::spawn(f.opts));
        REQUIRE_FALSE(spawned);
        auto* version = spawned.error().get<RpcError::Version>();
        REQUIRE(version);
        CHECK(version->offered == std::vector<std::uint32_t>{3, 4});
        CHECK(version->min == 1);
        CHECK(version->max == 2);
    }
    {
        auto f = fake("stderr config.yml is broken\nexit 1\n");
        auto spawned = block_on(exec, RpcClient::spawn(f.opts));
        REQUIRE_FALSE(spawned);
        auto* exited = spawned.error().get<RpcError::Exited>();
        REQUIRE(exited);
        CHECK(exited->status->code() == 1);
        CHECK(exited->stderr.find("config.yml is broken") != std::string::npos);
        auto missing = block_on(exec, RpcClient::spawn(SpawnOptions(f.dir.path() / "no-omp")));
        REQUIRE_FALSE(missing);
        CHECK(missing.error().get<RpcError::NotFound>());
    }
    {
        auto f = fake(std::string("> ") + std::string(loa::harness::testing::READY_V2) + "\n");
        REQUIRE(f.omp.set_version("19.1.0"));
        auto version = omp_version(f.omp.executable());
        REQUIRE(version);
        CHECK(*version == "19.1.0");
    }
}

TEST_CASE("omp rpc: concurrent requests match by id, keep codes, chunks and never reuse ids") {
    loa::runtime::Executor exec(1);
    const std::string body = R"({"type":"response","id":"c5","command":"get_state","success":true,"data":{"sessionId":"big"}})";
    auto chunk = [&](std::uint32_t index, std::string_view bytes) {
        const auto* ptr = reinterpret_cast<const std::uint8_t*>(bytes.data());
        return json::dump(Chunk{"k1", index, 2, body.size(), loa::core::base64::encode(std::span(ptr, bytes.size()))}.to_json());
    };
    const std::string script = loa::harness::testing::handshake_v2() +
        "< get_state a\n< get_available_models b\n" +
        reply("b", "get_available_models", R"({"models":[{"id":"m1","provider":"p"}]})") +
        reply("a", "get_state", R"({"sessionId":"s-1","isStreaming":false,"thinkingLevel":"high"})") +
        "< fork f\n> {\"type\":\"response\",\"id\":\"{{f}}\",\"command\":\"fork\",\"success\":false,\"error\":\"busy\",\"code\":\"session_busy\"}\n" +
        "< get_state\n> " + chunk(0, std::string_view(body).substr(0, 30)) + "\n> " + chunk(1, std::string_view(body).substr(30)) + "\n" +
        "< prompt p\n> {\"type\":\"response\",\"id\":\"{{p}}\",\"command\":\"prompt\",\"success\":true}\n";
    auto f = fake(script);
    auto spawned = block_on(exec, RpcClient::spawn(f.opts));
    REQUIRE(spawned);
    auto client = spawned->client;
    auto state_future = asio::co_spawn(exec.context(), client.request<SessionState>(command::GetState{}), asio::use_future);
    auto models_future = asio::co_spawn(exec.context(), client.request<ModelList>(command::GetAvailableModels{}), asio::use_future);
    auto state = state_future.get();
    auto models = models_future.get();
    REQUIRE(state);
    REQUIRE(models);
    CHECK(state->thinking_level == "high");
    CHECK(models->models[0].selector() == "p/m1");
    auto failed = block_on(exec, client.request(command::Fork{}));
    REQUIRE_FALSE(failed);
    CHECK(failed.error().code() == "session_busy");
    auto big = block_on(exec, client.request<SessionState>(command::GetState{}));
    REQUIRE(big);
    CHECK(big->session_id == "big");
    auto id = client.reserve_id();
    CHECK(client.reserve_id() != id);
    auto ack = block_on(exec, client.request_as<PromptAck>(id, command::Prompt{"hi", {}, StreamingBehavior::FollowUp}));
    REQUIRE(ack);
    CHECK_FALSE(ack->agent_invoked);
    REQUIRE(block_on(exec, client.shutdown(std::chrono::seconds(10))));
    auto sent = f.omp.received();
    const auto* prompt = sent.back().find("streamingBehavior");
    REQUIRE(prompt);
    CHECK(*prompt->as_string() == "followUp");
    CHECK(*sent.back().find("id")->as_string() == id);
}

TEST_CASE("omp rpc: events, UI and host callbacks flow in order until exit") {
    loa::runtime::Executor exec(3);
    auto f = fake(loa::harness::testing::handshake_v2() +
        "> {\"type\":\"agent_start\"}\n"
        "> {\"type\":\"message_update\",\"messageId\":\"m1\",\"message\":{\"role\":\"assistant\"},\"assistantMessageEvent\":{\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"Hi\"}}\n"
        "> {\"type\":\"future_event\",\"x\":1}\n"
        "> {\"type\":\"extension_ui_request\",\"id\":\"ui1\",\"method\":\"confirm\",\"title\":\"Go?\",\"message\":\"Continue?\"}\n"
        "< extension_ui_response\n< set_host_tools t\n" + reply("t", "set_host_tools", R"({"toolNames":["project_search"]})") +
        "> {\"type\":\"host_tool_call\",\"id\":\"h1\",\"toolCallId\":\"tc1\",\"toolName\":\"project_search\",\"arguments\":{\"q\":\"iced\"}}\n"
        "< host_tool_result\nstderr out of memory\nexit 9\n");
    f.opts.host = std::make_shared<Echo>();
    auto spawned = block_on(exec, RpcClient::spawn(f.opts));
    REQUIRE(spawned);
    auto client = spawned->client;
    auto& events = spawned->events;
    auto first = next(exec, events);
    REQUIRE(std::holds_alternative<incoming::Event>(first));
    CHECK(std::holds_alternative<event::AgentStart>(std::get<incoming::Event>(first).event));
    auto second = next(exec, events);
    REQUIRE(std::holds_alternative<incoming::Event>(second));
    auto* update = std::get_if<event::MessageUpdate>(&std::get<incoming::Event>(second).event);
    REQUIRE(update);
    CHECK(update->role == "assistant");
    CHECK(update->message_id == "m1");
    CHECK(std::get<assistant::TextDelta>(update->event).delta == "Hi");
    auto third = next(exec, events);
    REQUIRE(std::holds_alternative<incoming::Event>(third));
    auto* unknown = std::get_if<event::Unknown>(&std::get<incoming::Event>(third).event);
    REQUIRE(unknown);
    CHECK(*unknown->frame.find("x")->as_i64() == 1);
    auto fourth = next(exec, events);
    REQUIRE(std::holds_alternative<incoming::Ui>(fourth));
    const auto* dialog = std::get_if<ui::Confirm>(&std::get<incoming::Ui>(fourth).request);
    REQUIRE(dialog);
    REQUIRE(client.respond_ui(ui_response::Confirmed{dialog->id, true}));
    HostToolDefinition tool{"project_search", {}, "Search the project", json::Object{{"type", "object"}}, {}, {}};
    auto registered = block_on(exec, client.request(command::SetHostTools{{tool}}));
    REQUIRE(registered);
    CHECK(json::dump(*registered->find("toolNames")) == R"(["project_search"])");
    auto last = next(exec, events);
    REQUIRE(std::holds_alternative<incoming::Exited>(last));
    CHECK(std::get<incoming::Exited>(last).status->code() == 9);
    CHECK(std::get<incoming::Exited>(last).stderr.find("out of memory") != std::string::npos);
    CHECK(client.has_exited());
    auto failed = block_on(exec, client.request(command::Abort{}));
    CHECK_FALSE(failed);
    CHECK(failed.error().get<RpcError::Exited>());
    auto sent = f.omp.received();
    REQUIRE(sent.size() == 4);
    CHECK(json::dump(sent[1]) == R"({"type":"extension_ui_response","id":"ui1","confirmed":true})");
    CHECK(json::dump(*sent[2].find("tools")->as_array()->front().find("parameters")) == R"({"type":"object"})");
    CHECK(sent[3] == HostToolOutput::ok("project_search:iced").result_json("h1"));
}
