// omp sessions against a scripted fake omp: start, resume, fork, host tools, title rename,
// subagents, import and export (port of `tests/omp.rs`).
#include <doctest/doctest.h>

#include <algorithm>
#include <chrono>
#include <filesystem>
#include <string>
#include <thread>
#include <vector>

#include "../kit/support.hpp"
#include "loa/core/writer.hpp"
#include "loa/harness/omp/native.hpp"
#include "loa/harness/omp/oneshot.hpp"
#include "loa/harness/omp/session.hpp"
#include "loa/harness/registry/registry.hpp"
#include "loa/harness/testing/testing.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::core;
using namespace loa::contracts;
using namespace loa::harness::omp;
using loa::harness::test::block_on;
using loa::harness::test::read_file;
using loa::harness::testing::FakeProcess;
using loa::harness::testing::handshake_v2;
using loa::harness::testing::WatchSink;
namespace json = loa::runtime::json;
namespace fs = std::filesystem;

namespace {

std::string reply(std::string_view command, std::string_view data) {
    const std::string c(command);
    return "< " + c + " " + c + "\n> {\"type\":\"response\",\"id\":\"{{" + c + "}}\",\"command\":\"" + c +
           "\",\"success\":true,\"data\":" + std::string(data) + "}\n";
}

std::string setup(std::string_view state) {
    auto script = handshake_v2();
    for (auto command : {"set_event_filter", "set_subagent_subscription", "set_ask_dialog"}) script += reply(command, "{}");
    return script + reply("get_state", state);
}

std::string str(const json::Value& frame, std::string_view key) {
    const auto* value = frame.find(key);
    return value && value->as_string() ? *value->as_string() : std::string();
}

std::optional<std::string_view> root_attr(const Dom& dom, std::string_view name) {
    const auto* root = dom.root();
    return root ? root->str_attr(name) : std::nullopt;
}

struct Fixture {
    loa::runtime::Executor exec{3};
    loa::runtime::TempDir dir = loa::runtime::TempDir::create().value();
    FakeProcess fake;
    OmpAdapter adapter;
    explicit Fixture(const std::string& script)
        : fake(FakeProcess::install(dir.path(), script).value()), adapter(config()) {}
    OmpAdapterConfig config() const {
        OmpAdapterConfig c("owner", dir.path() / "state");
        c.executable = fake.executable();
        return c;
    }
    SessionConfig session(std::string id = "app-1") const {
        SessionConfig s;
        s.session_id = std::move(id);
        s.cwd = dir.path();
        return s;
    }
};

class Echo final : public HostTools {
  public:
    Awaitable<std::expected<std::string, std::string>> call(std::string session, std::string tool, std::string args) const override {
        co_return session + "/" + tool + ":" + args;
    }
};

} // namespace

TEST_CASE("omp adapter: a session streams, asks, settles and is titled against a fake omp") {
    auto script = setup(R"({"sessionId":"native-1","sessionFile":"/tmp/native-1.jsonl","isStreaming":false})");
    script += reply("prompt", R"({"agentInvoked":true})");
    auto update = [](std::string_view event) {
        return "> {\"type\":\"message_update\",\"messageId\":\"m1\",\"message\":{\"role\":\"assistant\"},\"assistantMessageEvent\":" +
               std::string(event) + "}\n";
    };
    script += "> {\"type\":\"agent_start\"}\n";
    script += update(R"({"type":"text_start","contentIndex":0})");
    script += update(R"({"type":"text_delta","contentIndex":0,"delta":"Working"})");
    script += "> {\"type\":\"tool_execution_start\",\"toolCallId\":\"call-1\",\"toolName\":\"read\",\"args\":{\"path\":\"a.rs\"}}\n";
    script += "> {\"type\":\"tool_execution_end\",\"toolCallId\":\"call-1\",\"toolName\":\"read\",\"result\":\"file text\"}\n";
    script += "> {\"type\":\"extension_ui_request\",\"id\":\"approval\",\"method\":\"confirm\",\"title\":\"Run?\",\"message\":\"write files\"}\n< extension_ui_response\n";
    script += update(R"({"type":"text_end","contentIndex":0,"content":"Working"})");
    script += "> {\"type\":\"prompt_result\",\"status\":\"completed\",\"agentInvoked\":true,\"sessionSettled\":true}\n";
    // The completed turn of an untitled session has omp title it.
    script += "< prompt rename\n> {\"type\":\"response\",\"id\":\"{{rename}}\",\"command\":\"prompt\",\"success\":true,\"data\":{\"agentInvoked\":false}}\n";
    script += "> {\"type\":\"session_info_update\",\"title\":\"Read a file\",\"sessionId\":\"native-1\"}\n";
    script += "> {\"type\":\"command_output\",\"text\":\"Session renamed to Read a file.\"}\n";
    script += "> {\"type\":\"thinking_level_changed\",\"thinkingLevel\":\"low\"}\n";
    Fixture f(script);
    auto sink = std::make_shared<WatchSink>();
    auto started = block_on(f.exec, f.adapter.start(f.session(), sink));
    REQUIRE_MESSAGE(started, (started ? std::string() : started.error().message()));
    auto& session = *started;
    REQUIRE(block_on(f.exec, session->send(Prompt{"Please read", {}, "owner"})));
    auto waiting = [](const Dom& dom) -> std::optional<NodeId> {
        for (const auto& n : dom.iter())
            if (n.kind == ElementKind::Decision) return n.id;
        return std::nullopt;
    };
    REQUIRE(sink->until("the approval", [&](const Dom& dom) { return waiting(dom).has_value(); }));
    // The state follows the DOM write that announced the decision.
    for (int i = 0; i < 200 && session->state() != SessionState::Waiting; ++i) std::this_thread::sleep_for(std::chrono::milliseconds(5));
    CHECK(session->state() == SessionState::Waiting);
    const auto decision = *sink->inner.with_dom(waiting);
    REQUIRE(block_on(f.exec, session->answer(decision, Answer{{"allow_once"}, std::nullopt, "owner"})));
    REQUIRE(block_on(f.exec, session->wait_idle()));
    REQUIRE(sink->until("the title", [](const Dom& dom) { return root_attr(dom, attr::EFFORT) == std::optional<std::string_view>("low"); }));
    const auto dom = sink->inner.dom();
    CHECK(root_attr(dom, attr::TITLE) == std::optional<std::string_view>("Read a file"));
    CHECK_MESSAGE(std::ranges::none_of(dom.iter(), [](const auto& n) {
                      return n.text.find("Session renamed") != std::string::npos || n.text == "/rename";
                  }), "the rename stays out of the transcript");
    CHECK(root_attr(dom, attr::NATIVE_ID) == std::optional<std::string_view>("native-1"));
    CHECK(root_attr(dom, native::X_SESSION_FILE) == std::optional<std::string_view>("/tmp/native-1.jsonl"));
    CHECK(std::ranges::any_of(dom.iter(), [](const auto& n) { return n.kind == ElementKind::Message && n.text == "Working"; }));
    CHECK(std::ranges::any_of(dom.iter(), [](const auto& n) { return n.kind == ElementKind::Result && n.text == "file text"; }));
    CHECK(std::ranges::any_of(dom.iter(), [](const auto& n) {
        return n.kind == ElementKind::Turn && n.str_attr(attr::STATUS) == std::optional<std::string_view>("done");
    }));
    REQUIRE(block_on(f.exec, session->shutdown()));
    const auto frames = f.fake.received();
    const auto approval = std::ranges::find_if(frames, [](const auto& frame) { return str(frame, "id") == "approval"; });
    REQUIRE(approval != frames.end());
    REQUIRE(approval->find("confirmed"));
    CHECK(json::dump(*approval->find("confirmed")) == "true");
    std::vector<std::string> prompts;
    for (const auto& frame : frames)
        if (str(frame, "type") == "prompt") prompts.push_back(str(frame, "message"));
    CHECK(prompts == std::vector<std::string>{"Please read", "/rename"});
    const auto args = f.fake.args();
    REQUIRE(args.size() >= 2);
    CHECK(args[0] == "--mode");
    CHECK(args[1] == "rpc-ui");
    const auto overlay = std::ranges::find(args, "--config");
    REQUIRE(overlay != args.end());
    CHECK(read_file(*std::next(overlay)) == MEMORY_OFF);
}

TEST_CASE("omp adapter: subagent transcripts and messages use the parent rpc") {
    auto script = setup(R"({"sessionId":"parent","isStreaming":false})");
    const std::string header = R"({"type":"session","version":3,"id":"child"})";
    const std::string message =
        R"({"type":"message","id":"5de0cb6e","parentId":null,"timestamp":"2026-11-26T04:18:15.732Z","message":{"role":"user","content":[{"type":"text","text":"PINEAPPLE-307"}],"steering":true}})";
    const std::string second =
        R"({"type":"message","id":"second","parentId":"5de0cb6e","message":{"role":"user","content":[{"type":"text","text":"SECOND"}]}})";
    struct Read { std::string agents, entries; int next; bool reset; };
    for (const auto& read : {Read{R"([{"id":"alpha","status":"running"}])", "[" + header + "," + message + "]", 100, false},
                             Read{R"([{"id":"alpha","status":"running"}])", "[" + second + "]", 200, false},
                             Read{"[]", "[" + header + "," + message + "]", 80, true}}) {
        script += reply("get_subagents", R"({"subagents":)" + read.agents + "}");
        script += reply("get_subagent_messages", "{\"nextByte\":" + std::to_string(read.next) + ",\"reset\":" +
                                                     (read.reset ? "true" : "false") + ",\"entries\":" + read.entries + "}");
    }
    script += reply("steer_subagent", R"({"success":true})");
    script += "< steer_subagent steer_subagent\n> {\"type\":\"response\",\"id\":\"{{steer_subagent}}\",\"command\":\"steer_subagent\",\"success\":false,\"error\":\"Subagent not running: alpha\"}\n";
    script += "< steer_subagent steer_subagent\n> {\"type\":\"response\",\"id\":\"{{steer_subagent}}\",\"command\":\"steer_subagent\",\"success\":false,\"error\":\"Subagent refused the message: busy\"}\n";
    Fixture f(script);
    auto started = block_on(f.exec, f.adapter.start(f.session("parent"), std::make_shared<MemorySink>()));
    REQUIRE_MESSAGE(started, (started ? std::string() : started.error().message()));
    auto& session = *started;
    auto has_message = [](const Dom& dom, std::string_view text) {
        return std::ranges::any_of(dom.iter(), [&](const auto& n) { return n.kind == ElementKind::Message && n.text == text; });
    };
    auto first = block_on(f.exec, session->subagent_transcript("alpha"));
    REQUIRE(first);
    CHECK(first->addressable);
    CHECK(has_message(first->dom, "PINEAPPLE-307"));
    auto next = block_on(f.exec, session->subagent_transcript("alpha"));
    REQUIRE(next);
    CHECK(has_message(next->dom, "SECOND"));
    auto reset = block_on(f.exec, session->subagent_transcript("alpha"));
    REQUIRE(reset);
    CHECK_FALSE(reset->addressable);
    CHECK_FALSE(has_message(reset->dom, "SECOND"));
    REQUIRE(block_on(f.exec, session->message_subagent("alpha", "hello")));
    auto gone = block_on(f.exec, session->message_subagent("alpha", "again"));
    REQUIRE_FALSE(gone);
    REQUIRE(gone.error().get<AdapterError::NotAddressable>());
    CHECK(gone.error().get<AdapterError::NotAddressable>()->reason == "alpha has finished, so omp no longer accepts messages for it");
    auto refused = block_on(f.exec, session->message_subagent("alpha", "again"));
    REQUIRE_FALSE(refused);
    REQUIRE(refused.error().get<AdapterError::Rejected>());
    CHECK(refused.error().get<AdapterError::Rejected>()->reason == "alpha refused the message: busy");
    auto empty = block_on(f.exec, session->message_subagent("alpha", "  "));
    REQUIRE_FALSE(empty);
    REQUIRE(empty.error().get<AdapterError::Protocol>());
    CHECK(empty.error().get<AdapterError::Protocol>()->detail == "empty message");
    REQUIRE(block_on(f.exec, session->shutdown()));
    std::vector<std::uint64_t> from;
    const auto frames = f.fake.received();
    for (const auto& frame : frames)
        if (str(frame, "type") == "get_subagent_messages") from.push_back(frame.find("fromByte")->as_u64().value_or(999));
    CHECK(from == std::vector<std::uint64_t>{0, 100, 200});
    const auto steer = std::ranges::find_if(frames, [](const auto& frame) { return str(frame, "type") == "steer_subagent"; });
    REQUIRE(steer != frames.end());
    CHECK(str(*steer, "subagentId") == "alpha");
    CHECK(str(*steer, "message") == "hello");
}

TEST_CASE("omp adapter: resume and fork reopen the native session file") {
    auto dom_with = [](std::optional<std::string> title) {
        auto sink = std::make_shared<MemorySink>();
        Attrs attrs{{"id", "s"}, {"harness", "omp"}, {"node", "owner"}, {"created", std::int64_t{1}},
                    {"schema", std::int64_t{1}}, {std::string(native::X_SESSION_FILE), "/tmp/old.jsonl"}};
        if (title) attrs.emplace("title", *title);
        REQUIRE(sink->commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, std::move(attrs), "")));
        return std::pair{sink->dom(), sink};
    };

    SUBCASE("resume") {
        Fixture f(setup(R"({"sessionId":"old","sessionFile":"/tmp/old.jsonl","isStreaming":false})"));
        auto [dom, sink] = dom_with("Kept");
        auto resumed = block_on(f.exec, f.adapter.resume(f.session(), dom, sink));
        REQUIRE_MESSAGE(resumed, (resumed ? std::string() : resumed.error().message()));
        CHECK((*resumed)->state() == SessionState::Idle);
        REQUIRE(block_on(f.exec, (*resumed)->shutdown()));
        const auto args = f.fake.args();
        const auto at = std::ranges::find(args, "--resume");
        REQUIRE(at != args.end());
        CHECK(*std::next(at) == "/tmp/old.jsonl");
        CHECK(root_attr(sink->dom(), attr::NATIVE_ID) == std::optional<std::string_view>("old"));
    }
    SUBCASE("fork") {
        auto script = setup(R"({"sessionId":"old","sessionFile":"/tmp/old.jsonl","isStreaming":false})");
        script += reply("fork", R"({"cancelled":false})");
        script += reply("get_state", R"({"sessionId":"new","sessionFile":"/tmp/new.jsonl","isStreaming":false})");
        Fixture f(script);
        auto [dom, sink] = dom_with(std::nullopt);
        auto forked = block_on(f.exec, f.adapter.fork(f.session(), dom, sink));
        REQUIRE_MESSAGE(forked, (forked ? std::string() : forked.error().message()));
        REQUIRE(block_on(f.exec, (*forked)->shutdown()));
        CHECK(root_attr(sink->dom(), native::X_SESSION_FILE) == std::optional<std::string_view>("/tmp/new.jsonl"));
    }
    SUBCASE("without a session file") {
        Fixture f(setup("{}"));
        MemorySink empty;
        REQUIRE(empty.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, Attrs{{"id", "s"}, {"harness", "omp"}, {"node", "owner"}, {"created", std::int64_t{1}}, {"schema", std::int64_t{1}}}, "")));
        auto resumed = block_on(f.exec, f.adapter.resume(f.session(), empty.dom(), std::make_shared<MemorySink>()));
        REQUIRE_FALSE(resumed);
        CHECK(resumed.error().get<AdapterError::Format>());
    }
}

TEST_CASE("omp adapter: host tools are registered and served through call_content") {
    auto script = handshake_v2();
    for (auto command : {"set_event_filter", "set_subagent_subscription", "set_ask_dialog"}) script += reply(command, "{}");
    script += reply("set_host_tools", R"({"toolNames":["project_search"]})");
    script += reply("get_state", R"({"sessionId":"h","isStreaming":false})");
    script += "> {\"type\":\"host_tool_call\",\"id\":\"h1\",\"toolCallId\":\"tc1\",\"toolName\":\"project_search\",\"arguments\":{\"q\":\"x\"}}\n";
    script += "< host_tool_result\n";
    Fixture f(script);
    auto session = f.session("app-h");
    session.host_tools.push_back(HostToolSpec{"project_search", "Search the project", R"({"type":"object"})"});
    session.host_tool_handler = std::make_shared<Echo>();
    auto started = block_on(f.exec, f.adapter.start(session, std::make_shared<MemorySink>()));
    REQUIRE_MESSAGE(started, (started ? std::string() : started.error().message()));
    // The fake reads the result before it waits for stdin to close.
    std::vector<json::Value> frames;
    for (int i = 0; i < 200; ++i) {
        frames = f.fake.received();
        if (std::ranges::any_of(frames, [](const auto& frame) { return str(frame, "type") == "host_tool_result"; })) break;
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    REQUIRE(block_on(f.exec, (*started)->shutdown()));
    frames = f.fake.received();
    const auto tools = std::ranges::find_if(frames, [](const auto& frame) { return str(frame, "type") == "set_host_tools"; });
    REQUIRE(tools != frames.end());
    const auto& tool = tools->find("tools")->as_array()->front();
    CHECK(str(tool, "name") == "project_search");
    CHECK(str(tool, "loadMode") == "essential");
    CHECK(json::dump(*tool.find("parameters")) == R"({"type":"object"})");
    const auto result = std::ranges::find_if(frames, [](const auto& frame) { return str(frame, "type") == "host_tool_result"; });
    REQUIRE(result != frames.end());
    CHECK_MESSAGE(json::dump(*result).find(R"(app-h/project_search:{\"q\":\"x\"})") != std::string::npos, json::dump(*result));
}

TEST_CASE("omp adapter: host tools without a handler are refused") {
    Fixture f(setup("{}"));
    auto session = f.session();
    session.host_tools.push_back(HostToolSpec{"t", "", "{}"});
    auto started = block_on(f.exec, f.adapter.start(session, std::make_shared<MemorySink>()));
    REQUIRE_FALSE(started);
    REQUIRE(started.error().get<AdapterError::Protocol>());
    CHECK(started.error().get<AdapterError::Protocol>()->detail == "host tools require a handler");
}

TEST_CASE("omp adapter: import records the native file and export writes it back") {
    auto dir = loa::runtime::TempDir::create().value();
    const OmpAdapter adapter(OmpAdapterConfig("owner", dir.path()));
    const auto native_file = fs::path(LOA_HARNESS_FIXTURES) / "omp/native/v1.jsonl";
    MemorySink sink;
    auto imported = adapter.import(native_file, sink);
    REQUIRE_MESSAGE(imported, (imported ? std::string() : imported.error().message()));
    const auto dom = sink.dom();
    CHECK(root_attr(dom, native::X_SESSION_FILE) == std::optional<std::string_view>(native_file.string()));
    CHECK(root_attr(dom, attr::NODE) == std::optional<std::string_view>("owner"));
    const auto out = dir.path() / "out.jsonl";
    REQUIRE(adapter.export_(dom, out));
    auto expected = native::export_string(dom);
    REQUIRE(expected);
    CHECK(read_file(out) == *expected);
}

TEST_CASE("omp adapter: the registry builds the omp adapter and account") {
    loa::harness::registry::HarnessEnv env{"owner", "/tmp/omp-state"};
    auto factories = loa::harness::registry::factories(env);
    const auto omp = std::ranges::find_if(factories, [](const auto& f) { return f->id() == HARNESS; });
    REQUIRE(omp != factories.end());
    auto adapter = std::dynamic_pointer_cast<OmpAdapter>((*omp)->build(fs::path("/opt/omp")));
    REQUIRE(adapter);
    CHECK(adapter->config.executable == fs::path("/opt/omp"));
    CHECK(adapter->config.node == "owner");
    CHECK(adapter->config.state_dir == fs::path("/tmp/omp-state"));
    CHECK(adapter->config.approval_mode == std::optional<std::string>("always-ask"));
    CHECK((*omp)->account(std::nullopt) != nullptr);
}
