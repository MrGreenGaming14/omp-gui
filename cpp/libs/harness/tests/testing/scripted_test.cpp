#include <doctest/doctest.h>

#include <fstream>

#include "../kit/support.hpp"
#include "loa/harness/testing/testing.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::core;
using namespace loa::harness::testing;
using loa::harness::test::block_on;
using loa::harness::test::golden;
using loa::harness::test::read_file;
using loa::contracts::Answer;
using loa::contracts::Prompt;

namespace {

loa::contracts::Prompt prompt(std::string text) { return Prompt{std::move(text), {}, "n1"}; }

NodeId last_decision(const WatchSink& sink) {
    return sink.inner.with_dom([](const Dom& d) {
        NodeId id;
        for (const Node& n : d.iter())
            if (n.kind == ElementKind::Decision) id = n.id;
        return id;
    });
}

class EchoTools final : public loa::contracts::HostTools {
  public:
    loa::contracts::Awaitable<std::expected<std::string, std::string>> call(std::string session, std::string tool, std::string args) const override {
        co_return session + ":" + tool + ":" + args;
    }
};

} // namespace

TEST_CASE("testing: ScriptedAdapter drives every prompt kind like the Rust double") {
    loa::runtime::Executor exec(2);
    ScriptedAdapter adapter("scripted");
    auto sink = std::make_shared<WatchSink>();
    loa::contracts::SessionConfig config;
    config.session_id = "s1";
    config.node = "n1";
    config.cwd = "/work";
    config.project = "p";
    auto session = block_on(exec, adapter.start(config, sink)).value();
    REQUIRE(block_on(exec, session->send(prompt("hello"))));
    REQUIRE(block_on(exec, session->send(prompt("ask question"))));
    CHECK(session->state() == loa::contracts::SessionState::Idle);
    REQUIRE(block_on(exec, session->answer(last_decision(*sink), Answer{{"red"}, "why", "n1"})));
    REQUIRE(block_on(exec, session->send(prompt("ask multi"))));
    REQUIRE(block_on(exec, session->answer(last_decision(*sink), Answer{{"rust", "zig"}, std::nullopt, "n2"})));
    REQUIRE(block_on(exec, session->send(prompt("ask plan"))));
    REQUIRE(block_on(exec, session->answer(last_decision(*sink), Answer{{"approve"}, std::nullopt, "n1"})));
    REQUIRE(block_on(exec, session->send(prompt("ask form"))));
    REQUIRE(block_on(exec, session->answer(last_decision(*sink), Answer{{R"({"name":"r1","channel":"beta"})"}, std::nullopt, "n1"})));
    REQUIRE(block_on(exec, session->send(prompt("ask"))));
    REQUIRE(block_on(exec, session->answer(last_decision(*sink), Answer{{"allow_once"}, std::nullopt, "n1"})));
    REQUIRE(block_on(exec, session->send(prompt("work bash Build it"))));
    REQUIRE(block_on(exec, session->interrupt()));
    REQUIRE(block_on(exec, session->send(prompt("tool x {}"))));
    CHECK(sink->inner.with_dom([](const Dom& d) { return render(d); }) == read_file(golden("harness/scripted.txt")));

    std::string rules;
    for (const auto& v : sink->inner.with_dom([](const Dom& d) { return check_dom(d); }))
        rules += std::string(to_string(v.rule)) + " " + std::to_string(v.node ? v.node->value : 0) + "\n";
    for (const auto& v : check_history(sink->inner.patches()))
        rules += "history " + std::string(to_string(v.rule)) + " " + std::to_string(v.node ? v.node->value : 0) + "\n";
    CHECK(rules == read_file(golden("harness/scripted_violations.txt")));

    auto unknown = block_on(exec, session->answer(NodeId{99}, Answer{}));
    REQUIRE_FALSE(unknown);
    CHECK(unknown.error().get<loa::contracts::AdapterError::UnknownDecision>());
    const auto seen = adapter.seen();
    CHECK(seen.prompts.size() == 8);
    CHECK(seen.interrupts == 1);
    CHECK(seen.configs.size() == 1);
    CHECK(seen.configs[0].session_id == "s1");
}

TEST_CASE("testing: ScriptedAdapter hooks: tools, models, effort, modes, resume, subagents") {
    loa::runtime::Executor exec(2);
    const std::vector<ScriptedModel> models{{"anthropic/opus", {"low", "high"}}};
    auto adapter = ScriptedAdapter("scripted").with_models(models).with_effort().subagent_messages();
    auto sink = std::make_shared<WatchSink>();
    loa::contracts::SessionConfig config;
    config.session_id = "s";
    config.node = "me";
    config.host_tool_handler = std::make_shared<EchoTools>();
    auto session = block_on(exec, adapter.start(config, sink)).value();
    REQUIRE(block_on(exec, session->send(prompt("tool look {\"a\":1}"))));
    REQUIRE(sink->until("the tool reply", [](const Dom& d) {
        for (const Node& n : d.iter())
            if (n.text == "s:look:{\"a\":1}") return true;
        return false;
    }));
    CHECK(adapter.seen().configs[0].host_tool_handler == nullptr);
    auto listed = block_on(exec, session->models()).value();
    REQUIRE(listed.size() == 1);
    CHECK(listed[0].provider == "anthropic");
    CHECK(listed[0].efforts == std::vector<std::string>{"low", "high"});
    REQUIRE(block_on(exec, session->set_model({"anthropic/opus", std::nullopt})));
    CHECK_FALSE(block_on(exec, session->set_model({"nope", std::nullopt})));
    REQUIRE(block_on(exec, session->set_effort("high")));
    REQUIRE(block_on(exec, session->set_mode(SessionMode::Plan)));
    REQUIRE(block_on(exec, session->compact("short")));
    const auto root = sink->inner.with_dom([](const Dom& d) { return *d.root(); });
    CHECK(root.str_attr(attr::MODEL) == "anthropic/opus");
    CHECK(root.str_attr(attr::EFFORT) == "high");
    CHECK(root.str_attr(attr::MODE) == "plan");

    auto missing = block_on(exec, session->message_subagent("ghost", "hi"));
    REQUIRE_FALSE(missing);
    CHECK(missing.error().get<loa::contracts::AdapterError::NotAddressable>());
    adapter.set_subagent_transcript("sub", Dom{}, true);
    REQUIRE(block_on(exec, session->message_subagent("sub", "steer")));
    auto transcript = block_on(exec, session->subagent_transcript("sub")).value();
    CHECK(transcript.addressable);
    CHECK(transcript.dom.size() == 3);
    adapter.set_subagent_error("sub", "broken", std::nullopt);
    CHECK_FALSE(block_on(exec, session->subagent_transcript("sub")));
    const auto seen = adapter.seen();
    CHECK(seen.subagent_messages == std::vector<std::pair<std::string, std::string>>{{"sub", "steer"}});
    CHECK(seen.compacts == std::vector<std::optional<std::string>>{"short"});

    const Dom dom = sink->inner.dom();
    auto resumed = block_on(exec, adapter.resume(config, dom, sink));
    REQUIRE(resumed);
    CHECK(adapter.seen().resumes == 1);
    auto refused = block_on(exec, ScriptedAdapter("x").no_resume().resume(config, dom, sink));
    CHECK(refused.error().get<loa::contracts::AdapterError::Unsupported>());
    auto absent = block_on(exec, ScriptedAdapter("x").no_start().start(config, std::make_shared<MemorySink>()));
    CHECK(absent.error().get<loa::contracts::AdapterError::NotInstalled>());
    auto plain = ScriptedAdapter("x");
    auto plain_session = block_on(exec, plain.start(config, std::make_shared<MemorySink>())).value();
    CHECK(block_on(exec, plain_session->set_effort("x")).error().get<loa::contracts::AdapterError::Unsupported>());
    CHECK(block_on(exec, plain_session->models()).error().get<loa::contracts::AdapterError::Unsupported>());
    CHECK(block_on(exec, plain.availability()).version == "1.0");
}

namespace {

class ScriptedCodec final : public loa::harness::kit::NativeCodec {
  public:
    loa::contracts::Result<> import(const std::filesystem::path& p, PatchSink& sink) const override { return adapter.import(p, sink); }
    loa::contracts::Result<> export_(const Dom& d, const std::filesystem::path& out) const override { return adapter.export_(d, out); }
    std::vector<loa::harness::kit::NativeSession> discover(const std::filesystem::path&) const override { return {}; }
    ScriptedAdapter adapter{"scripted"};
};

struct EchoMapper {
    using Reply = std::string;
    std::expected<std::vector<std::string>, loa::contracts::AdapterError> apply(std::string ev, const DomWriter&) {
        if (ev == "\"bad\"") return std::unexpected(loa::contracts::AdapterError(loa::contracts::AdapterError::Protocol{"bad event"}));
        return std::vector<std::string>{ev};
    }
};

} // namespace

TEST_CASE("testing: round_trip, replay, golden and render") {
    auto dir = loa::runtime::TempDir::create().value();
    const auto native = dir.path() / "s.txt";
    std::ofstream(native) << "one\ntwo";
    ScriptedCodec codec;
    CHECK(round_trip(codec, native));

    const auto fixture = dir.path() / "events.jsonl";
    std::ofstream(fixture) << "# comment\n\n \"a\" \n\"b\"\n";
    MemorySink sink;
    EchoMapper mapper;
    auto decode = [](std::string_view line) -> std::expected<std::string, std::string> { return std::string(line); };
    CHECK(replay(mapper, sink, fixture, decode).value() == std::vector<std::string>{"\"a\"", "\"b\""});
    std::ofstream(fixture) << "\"bad\"\n";
    CHECK(replay(mapper, sink, fixture, decode).error() == "\"bad\": harness protocol error: bad event");

    const auto file = dir.path() / "g.txt";
    CHECK_FALSE(golden(file, "x"));
    ::setenv("LOA_BLESS", "1", 1);
    CHECK(golden(file, "blessed"));
    ::unsetenv("LOA_BLESS");
    CHECK(read_file(file) == "blessed");
    CHECK(golden(file, "blessed"));
    CHECK(golden(file, "other").error() == file.string() + " differs (LOA_BLESS=1 rewrites it)");
}

TEST_CASE("testing: FakeProcess runs its script under bash") {
    auto dir = loa::runtime::TempDir::create().value();
    auto fake = FakeProcess::install(dir.path(), handshake_v2() + "< prompt p\n> {\"type\":\"done\",\"id\":\"{{p}}\"}\nstderr bye\nexit 4\n").value();
    REQUIRE(fake.set_version("20.1.0"));
    auto version = loa::runtime::exec::output(loa::runtime::exec::command(fake.executable(), {"--version"})).value();
    CHECK(version.out == "20.1.0\n");

    const std::string input = "{\"type\":\"negotiate_protocol\",\"id\":\"n-7\"}\n{\"type\":\"prompt\",\"id\":42}\n";
    const auto in = dir.path() / "in.txt";
    std::ofstream(in) << input;
    auto run = loa::runtime::exec::output(loa::runtime::exec::command(
                   "/bin/sh", {"-c", loa::runtime::exec::shell_quote(fake.executable().string()) + " a 'b c' < " +
                                         loa::runtime::exec::shell_quote(in.string())}))
                   .value();
    CHECK(run.status.code() == 4);
    CHECK(run.out == std::string(READY_V2) +
                         "\n{\"type\":\"response\",\"id\":\"n-7\",\"command\":\"negotiate_protocol\",\"success\":true,\"data\":{\"protocolVersion\":2}}\n"
                         "{\"type\":\"done\",\"id\":\"42\"}\n");
    CHECK(run.err == "bye\n");
    CHECK(fake.args() == std::vector<std::string>{"a", "b c"});
    const auto received = fake.received();
    REQUIRE(received.size() == 2);
    CHECK(*received[1].find("id")->as_i64() == 42);

    auto wrong = FakeProcess::install(dir.path(), "< prompt\n").value();
    std::ofstream(in) << "{\"type\":\"other\"}\n";
    auto failed = loa::runtime::exec::output(loa::runtime::exec::command(
                      "/bin/sh", {"-c", loa::runtime::exec::shell_quote(wrong.executable().string()) + " < " + loa::runtime::exec::shell_quote(in.string())}))
                      .value();
    CHECK(failed.status.code() == 3);
    CHECK(failed.err == "fake harness: expected prompt, got {\"type\":\"other\"}\n");
}
