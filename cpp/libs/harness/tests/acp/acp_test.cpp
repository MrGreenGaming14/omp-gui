// ACP transcript replays, host confinement, file URIs and a session with a fake agent
// (port of `tests/acp.rs`).
#include <doctest/doctest.h>

#include <algorithm>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <string>
#include <vector>

#include <unistd.h>

#include "../kit/support.hpp"
#include "loa/core/xml.hpp"
#include "loa/harness/acp/acp.hpp"
#include "loa/harness/registry/registry.hpp"
#include "loa/harness/testing/testing.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::core;
using namespace loa::harness::acp;
using loa::harness::test::block_on;
using loa::harness::test::read_file;
namespace json = loa::runtime::json;
namespace fs = std::filesystem;
using json::Array;
using json::Object;

namespace {

Json parse(std::string_view text) { return json::parse(text).value(); }

NodeId session_root(PatchSink& sink) {
    Attrs attrs{{"id", Value("s")}, {"harness", Value("acp")}, {"node", Value("owner")},
                {"created", Value(std::int64_t{1})}, {"schema", Value(std::int64_t{1})}};
    return sink.commit_at(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, std::move(attrs), ""), 0).value().node;
}

loa::contracts::Prompt prompt(std::string text) { return loa::contracts::Prompt{std::move(text), {}, "owner"}; }

loa::harness::kit::RpcResult<Json> serve(loa::runtime::Executor& exec, const Host& host, std::string method, std::string_view params) {
    return block_on(exec, host.serve(std::move(method), parse(params)));
}

std::string text_of(const Json& v) { return json::dump(v); }

} // namespace

TEST_CASE("acp: transcripts replay to their goldens") {
    const fs::path dir = fs::path(LOA_HARNESS_FIXTURES) / "acp";
    std::vector<fs::path> fixtures;
    for (const auto& entry : fs::directory_iterator(dir))
        if (entry.path().extension() == ".jsonl") fixtures.push_back(entry.path());
    std::ranges::sort(fixtures);
    REQUIRE_FALSE(fixtures.empty());
    for (const auto& fixture : fixtures) {
        CAPTURE(fixture.string());
        MemorySink sink;
        AcpMapper mapper(session_root(sink), std::string("fake-model"));
        mapper.adopt_native("s-1");
        auto replies = loa::harness::testing::replay(mapper, sink, fixture, [](std::string_view line) -> std::expected<Input, std::string> {
            auto raw = json::parse(line);
            if (!raw) return std::unexpected(raw.error().message());
            return decode_fixture(std::move(*raw));
        });
        REQUIRE_MESSAGE(replies, replies.error());
        std::string out;
        const std::string xml = loa::core::xml::to_string(sink.dom());
        std::size_t start = 0;
        while (start < xml.size()) {
            std::size_t end = xml.find('\n', start);
            if (end == std::string::npos) end = xml.size();
            std::string_view line(xml.data() + start, end - start);
            if (!line.starts_with("<!--")) {
                out.append(line);
                out.push_back('\n');
            }
            start = end + 1;
        }
        out += "\n--- replies\n";
        for (const auto& reply : *replies) {
            if (const auto* respond = std::get_if<reply::Respond>(&reply))
                out += text_of(Json(Object{{"id", respond->id}, {"result", respond->result}})) + "\n";
            else if (const auto* reject = std::get_if<reply::Reject>(&reply))
                out += text_of(Json(Object{{"id", reject->id}, {"error", reject->message}})) + "\n";
            else
                out += "notify session/cancel\n";
        }
        auto same = loa::harness::testing::golden(fs::path(fixture).replace_extension(".golden"), out);
        CHECK_MESSAGE(same, (same ? "" : same.error()), "\n", out);
    }
}

TEST_CASE("acp: host refuses paths outside the session directory") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    const fs::path root = dir.path() / "root";
    fs::create_directory(root);
    const fs::path outside = dir.path() / "secret";
    std::ofstream(outside) << "secret";
    auto host = Host::create(root).value();
    REQUIRE(serve(exec, *host, "fs/write_text_file", R"({"path":"nested/file","content":"one\ntwo\n"})"));
    auto read = serve(exec, *host, "fs/read_text_file", R"({"path":"nested/file","line":2,"limit":1})");
    REQUIRE(read);
    CHECK(text_of(*read) == R"({"content":"two\n"})");
    for (const std::string path : {std::string("../secret"), outside.string()}) {
        const std::string quoted = text_of(Json(path));
        CHECK_FALSE(serve(exec, *host, "fs/read_text_file", R"({"path":)" + quoted + "}"));
        CHECK_FALSE(serve(exec, *host, "fs/write_text_file", R"({"path":)" + quoted + R"(,"content":"overwritten"})"));
        CHECK_FALSE(serve(exec, *host, "terminal/create", R"({"command":"true","cwd":)" + quoted + "}"));
    }
    fs::create_directory_symlink(dir.path(), root / "escape");
    CHECK_FALSE(serve(exec, *host, "fs/read_text_file", R"({"path":"escape/secret"})"));
    CHECK_FALSE(serve(exec, *host, "fs/write_text_file", R"({"path":"escape/new","content":"bad"})"));
    CHECK_FALSE(serve(exec, *host, "terminal/create", R"({"command":"true","cwd":"escape"})"));
    CHECK_FALSE(fs::exists(dir.path() / "new"));
    CHECK(read_file(outside) == "secret");
}

TEST_CASE("acp: Host::resolve confines absolute, relative and .. paths") {
    auto dir = loa::runtime::TempDir::create().value();
    auto host = Host::create(dir.path()).value();
    const fs::path& root = host->root();
    CHECK(host->resolve("a/b").value() == root / "a" / "b");
    CHECK(host->resolve((root / "a").string()).value() == root / "a");
    CHECK(host->resolve(root.string()).value() == root);
    auto up = host->resolve("../x");
    REQUIRE_FALSE(up);
    CHECK(up.error().code() == loa::ErrorCode::permission_denied);
    auto absolute = host->resolve("/etc/passwd");
    REQUIRE_FALSE(absolute);
    CHECK(absolute.error().code() == loa::ErrorCode::permission_denied);
    CHECK(absolute.error().message() == "/etc/passwd is outside the session directory");
    auto escaping = host->resolve((root / ".." / "x").string());
    CHECK_FALSE(escaping);
}

TEST_CASE("acp: host terminal runs and releases inside the session") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto host = Host::create(dir.path()).value();
    auto created = serve(exec, *host, "terminal/create", R"({"command":"sh","args":["-c","printf 'hello'"]})");
    REQUIRE(created);
    const std::string terminal = text_of(Json(Object{{"terminalId", *created->find("terminalId")}}));
    auto exit = serve(exec, *host, "terminal/wait_for_exit", terminal);
    REQUIRE(exit);
    CHECK(text_of(*exit) == R"({"exitCode":0,"signal":null})");
    auto output = serve(exec, *host, "terminal/output", terminal);
    REQUIRE(output);
    CHECK(*output->find("output")->as_string() == "hello");
    CHECK(*output->find("truncated")->as_bool() == false);
    REQUIRE(serve(exec, *host, "terminal/release", terminal));
    CHECK_FALSE(serve(exec, *host, "terminal/output", terminal));
}

TEST_CASE("acp: host terminal reports exit, env, cwd, kill and spawn failures") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto host = Host::create(dir.path()).value();
    fs::create_directory(host->root() / "sub");
    auto created = serve(exec, *host, "terminal/create",
                         R"({"command":"sh","args":["-c","pwd; echo $GREETING 1>&2; exit 3"],"cwd":"sub","env":[{"name":"GREETING","value":"hi"}],"outputByteLimit":1000})");
    REQUIRE(created);
    const std::string terminal = text_of(Json(Object{{"terminalId", *created->find("terminalId")}}));
    auto exit = serve(exec, *host, "terminal/wait_for_exit", terminal);
    REQUIRE(exit);
    CHECK(text_of(*exit) == R"({"exitCode":3,"signal":null})");
    auto output = serve(exec, *host, "terminal/output", terminal);
    REQUIRE(output);
    CHECK(*output->find("output")->as_string() == (host->root() / "sub").string() + "\nhi\n");
    CHECK(text_of(*output->find("exitStatus")) == R"({"exitCode":3,"signal":null})");

    auto sleeper = serve(exec, *host, "terminal/create", R"({"command":"sleep","args":["600"]})");
    REQUIRE(sleeper);
    const std::string sleeping = text_of(Json(Object{{"terminalId", *sleeper->find("terminalId")}}));
    REQUIRE(serve(exec, *host, "terminal/kill", sleeping));
    auto killed = serve(exec, *host, "terminal/wait_for_exit", sleeping);
    REQUIRE(killed);
    CHECK(text_of(*killed) == R"({"exitCode":null,"signal":"SIGKILL"})");

    auto missing = serve(exec, *host, "terminal/create", R"({"command":"loa-no-such-command"})");
    REQUIRE_FALSE(missing);
    CHECK(missing.error().message().find("can't run `loa-no-such-command`") != std::string::npos);
    auto unknown = serve(exec, *host, "terminal/frobnicate", "{}");
    REQUIRE_FALSE(unknown);
    CHECK(unknown.error().code() == "-32601");
}

TEST_CASE("acp: file_uri percent-encodes spaces and non-ASCII bytes") {
    CHECK(file_uri("/tmp/a b/caf\xC3\xA9.txt") == "file:///tmp/a%20b/caf%C3%A9.txt");
    CHECK(file_uri("/x/A-z_0.9~/%#?") == "file:///x/A-z_0.9~/%25%23%3F");
    loa::contracts::Prompt p{"look", {{"/w/my file.rs", std::nullopt}, {"/w/img.png", std::string("image/png")}}, "owner"};
    const auto blocks = prompt_blocks(p);
    REQUIRE(blocks.size() == 3);
    CHECK(text_of(blocks[0]) == R"({"type":"text","text":"look"})");
    CHECK(text_of(blocks[1]) == R"({"type":"resource_link","name":"my file.rs","uri":"file:///w/my%20file.rs"})");
    CHECK(text_of(blocks[2]) == R"({"type":"resource_link","name":"img.png","uri":"file:///w/img.png","mimeType":"image/png"})");
}

TEST_CASE("acp: session streams and settles against a fake agent") {
    const std::string script =
        "< initialize init\n"
        "> {\"jsonrpc\":\"2.0\",\"id\":\"{{init}}\",\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{}}}\n"
        "< session/new new\n"
        "> {\"jsonrpc\":\"2.0\",\"id\":\"{{new}}\",\"result\":{\"sessionId\":\"native-1\"}}\n"
        "< session/prompt prompt\n"
        "> {\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"native-1\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"Hello\"}}}}\n"
        "> {\"jsonrpc\":\"2.0\",\"id\":\"{{prompt}}\",\"result\":{\"stopReason\":\"end_turn\"}}\n";
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto fake = loa::harness::testing::FakeProcess::install(dir.path(), script).value();
    AcpAdapter adapter(fake.executable(), {});
    auto sink = std::make_shared<loa::harness::testing::WatchSink>();
    loa::contracts::SessionConfig config;
    config.session_id = "app-1";
    config.cwd = dir.path();
    auto session = block_on(exec, adapter.start(config, sink));
    REQUIRE_MESSAGE(session, (session ? "" : session.error().message()));
    REQUIRE(block_on(exec, (*session)->send(prompt("Say hello"))));
    REQUIRE(block_on(exec, (*session)->wait_idle()));
    const Dom dom = sink->inner.dom();
    CHECK(dom.root()->str_attr(attr::NATIVE_ID) == "native-1");
    bool hello = false, done = false;
    for (const auto& n : dom.iter()) {
        hello |= n.kind == ElementKind::Message && n.text == "Hello";
        done |= n.kind == ElementKind::Turn && n.str_attr(attr::STATUS) == "done";
    }
    CHECK(hello);
    CHECK(done);
    REQUIRE(block_on(exec, (*session)->shutdown()));
    const auto sent = fake.received();
    REQUIRE(sent.size() >= 3);
    CHECK(*sent[0].find("method")->as_string() == "initialize");
    CHECK(*sent[1].find("method")->as_string() == "session/new");
    CHECK(*sent[2].find("params")->find("sessionId")->as_string() == "native-1");
    CHECK(*(*sent[2].find("params")->find("prompt")->as_array())[0].find("text")->as_string() == "Say hello");
    CHECK(adapter.capabilities() == AcpAdapter::base_capabilities());
}

TEST_CASE("acp: model and mode changes land only once the agent answers") {
    const std::string script =
        "< initialize init\n"
        "> {\"jsonrpc\":\"2.0\",\"id\":\"{{init}}\",\"result\":{\"protocolVersion\":1,\"agentInfo\":{\"name\":\"fake\",\"version\":\"1.2\"}}}\n"
        "< session/new new\n"
        "> {\"jsonrpc\":\"2.0\",\"id\":\"{{new}}\",\"result\":{\"sessionId\":\"native-1\",\"models\":{\"currentModelId\":\"m1\",\"availableModels\":[{\"modelId\":\"m1\"},{\"modelId\":\"m2\",\"name\":\"Two\"}]},\"modes\":{\"currentModeId\":\"default\",\"availableModes\":[{\"id\":\"default\"},{\"id\":\"plan\"}]}}}\n"
        "< session/set_model model\n"
        "> {\"jsonrpc\":\"2.0\",\"id\":\"{{model}}\",\"result\":{}}\n"
        "< session/set_mode mode\n"
        "> {\"jsonrpc\":\"2.0\",\"id\":\"{{mode}}\",\"error\":{\"code\":-32603,\"message\":\"no plans today\"}}\n";
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto fake = loa::harness::testing::FakeProcess::install(dir.path(), script).value();
    AcpAdapter adapter(fake.executable(), {});
    auto sink = std::make_shared<MemorySink>();
    loa::contracts::SessionConfig config;
    config.cwd = dir.path();
    auto session = block_on(exec, adapter.start(config, sink));
    REQUIRE_MESSAGE(session, (session ? "" : session.error().message()));
    auto root = [&] { return sink->dom(); };
    CHECK(root().root()->str_attr(attr::MODEL) == "m1");
    CHECK(root().root()->str_attr(X_MODE) == "default");
    CHECK(root().root()->str_attr(X_AGENT) == "fake 1.2");
    auto caps = adapter.capabilities();
    CHECK(caps.has(loa::contracts::Feature::PlanMode));
    CHECK(caps.model_switch == loa::contracts::ModelSwitch::PerTurn);

    auto models = block_on(exec, (*session)->models());
    REQUIRE(models);
    REQUIRE(models->size() == 2);
    CHECK((*models)[0].name == "m1");
    CHECK((*models)[1].name == "Two");
    CHECK((*models)[1].provider == "acp");

    auto refused = block_on(exec, (*session)->set_model({"m3", std::nullopt}));
    REQUIRE_FALSE(refused);
    CHECK(refused.error().get<loa::contracts::AdapterError::Protocol>());
    CHECK(refused.error().message().find("the agent doesn't offer model m3") != std::string::npos);
    auto role = block_on(exec, (*session)->set_model({"m2", std::string("smol")}));
    REQUIRE_FALSE(role);
    CHECK(role.error().get<loa::contracts::AdapterError::Unsupported>());
    CHECK(root().root()->str_attr(attr::MODEL) == "m1");

    REQUIRE(block_on(exec, (*session)->set_model({"m2", std::nullopt})));
    CHECK(root().root()->str_attr(attr::MODEL) == "m2");

    auto mode = block_on(exec, (*session)->set_mode(SessionMode::Plan));
    REQUIRE_FALSE(mode);
    CHECK(root().root()->str_attr(X_MODE) == "default");
    CHECK(root().root()->str_attr(attr::MODE) == "normal");
    REQUIRE(block_on(exec, (*session)->set_mode(SessionMode::Normal)));
    REQUIRE(block_on(exec, (*session)->shutdown()));
    CHECK((*session)->state() == loa::contracts::SessionState::Closed);
}

TEST_CASE("acp: availability reports the agent and the missing hint") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto fake = loa::harness::testing::FakeProcess::install(dir.path(),
        "< initialize init\n"
        "> {\"jsonrpc\":\"2.0\",\"id\":\"{{init}}\",\"result\":{\"protocolVersion\":2}}\n").value();
    auto incompatible = block_on(exec, AcpAdapter(fake.executable(), {}).availability());
    CHECK(incompatible.status == loa::contracts::AvailabilityStatus::Incompatible);
    CHECK(incompatible.detail == "the agent speaks ACP version 2, this client speaks 1");

    auto missing = block_on(exec, AcpAdapter(dir.path() / "nope", {}).with_missing_hint("install one").availability());
    CHECK(missing.status == loa::contracts::AvailabilityStatus::NotInstalled);
    CHECK(missing.detail == "install one");
}

TEST_CASE("acp: permission options advertise canonical answers and keep name or id answers") {
    MemorySink sink;
    AcpMapper mapper(session_root(sink), std::nullopt);
    const DomWriter w = DomWriter(sink).at(0);
    REQUIRE(mapper.apply(input::Prompt{"1", prompt("go")}, w));
    auto ask = [](std::int64_t id, std::string_view options) {
        return input::Permission{Json(id), parse(std::format(R"({{"toolCall":{{"toolCallId":"c{}","title":"run"}},"options":{}}})", id, options))};
    };
    const std::string full = R"([{"optionId":"opt-once","name":"Allow","kind":"allow_once"},{"optionId":"opt-always","name":"Always allow","kind":"allow_always"},{"optionId":"opt-no","name":"Reject","kind":"reject_once"}])";
    const std::string once_only = R"([{"optionId":"opt-once","name":"Allow","kind":"allow_once"},{"optionId":"opt-no","name":"Reject","kind":"reject_once"}])";
    REQUIRE(mapper.apply(ask(1, full), w));
    REQUIRE(mapper.apply(ask(2, full), w));
    REQUIRE(mapper.apply(ask(3, once_only), w));

    std::vector<std::pair<NodeId, std::vector<std::string_view>>> decisions;
    const Dom dom = sink.dom();
    for (const auto& n : dom.iter())
        if (n.kind == ElementKind::Decision) decisions.emplace_back(n.id, n.attr(attr::OPTIONS)->str_items());
    REQUIRE(decisions.size() == 3);
    using SV = std::vector<std::string_view>;
    CHECK(decisions[0].second == SV{answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY});
    CHECK(decisions[1].second == SV{answer::ALLOW_ONCE, answer::ALLOW_SESSION, answer::DENY});
    CHECK(decisions[2].second == SV{answer::ALLOW_ONCE, answer::DENY});

    auto selected = [](std::int64_t id, std::string_view option) {
        return Reply(reply::Respond{Json(id), Json(Object{{"outcome", Object{{"optionId", option}, {"outcome", "selected"}}}})});
    };
    auto by_id = mapper.answer(decisions[0].first, {{"opt-always"}, std::nullopt, "owner"}, w);
    REQUIRE(by_id);
    CHECK(*by_id == std::optional<Reply>(selected(1, "opt-always")));
    auto by_name = mapper.answer(decisions[1].first, {{"Always allow"}, std::nullopt, "owner"}, w);
    REQUIRE(by_name);
    CHECK(*by_name == std::optional<Reply>(selected(2, "opt-always")));
    auto by_canonical = mapper.answer(decisions[2].first, {{std::string(answer::ALLOW_ONCE)}, std::nullopt, "owner"}, w);
    REQUIRE(by_canonical);
    CHECK(*by_canonical == std::optional<Reply>(selected(3, "opt-once")));
}

TEST_CASE("acp: registry factory constructs the adapter") {
    register_factory();
    auto builds = loa::harness::registry::factories({"owner", "/tmp/omp"});
    const auto it = std::ranges::find_if(builds, [](const auto& f) { return f->id() == "acp"; });
    REQUIRE(it != builds.end());
    auto adapter = (*it)->build(fs::path("/opt/gemini"));
    REQUIRE(adapter);
    CHECK(adapter->harness() == "acp");
    CHECK(adapter->capabilities() == AcpAdapter::base_capabilities());
}
