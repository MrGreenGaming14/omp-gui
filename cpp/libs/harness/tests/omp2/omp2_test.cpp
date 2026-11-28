// omp2 transcript replays and RPC adapter behavior.
#include <doctest/doctest.h>

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

#include "../kit/support.hpp"
#include "loa/core/writer.hpp"
#include "loa/harness/omp2/omp2.hpp"
#include "loa/harness/testing/testing.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::core;
using namespace loa::harness::omp2;
using namespace loa::harness::testing;
using loa::harness::test::block_on;
using loa::harness::test::golden;
using loa::harness::test::read_file;
namespace json = loa::runtime::json;
namespace fs = std::filesystem;

namespace {

// The fixture control records are not omp2 frames: they drive the same mapper calls as a session.
void replay_fixture(const fs::path& fixture) {
    MemorySink sink;
    const auto root = sink.commit_at(Draft::insert(NodeId::DOCUMENT, ElementKind::Session,
                                                    Attrs{{"id", "s"}, {"harness", "omp2"}, {"node", "owner"}, {"created", std::int64_t{1}}}, ""), 0);
    REQUIRE(root);
    Omp2Mapper mapper(root->node);
    const DomWriter writer = DomWriter(sink).at(0);
    std::vector<wire::Reply> replies;
    std::ifstream input(fixture);
    REQUIRE(input.is_open());
    std::string line;
    while (std::getline(input, line)) {
        const auto start = line.find_first_not_of(" \t\r\n");
        if (start == std::string::npos || line[start] == '#') continue;
        const auto parsed = json::parse(line);
        REQUIRE_MESSAGE(parsed, fixture.string(), ": ", line);
        const auto* control = parsed->find("$");
        if (control && control->as_string() && *control->as_string() == "send") {
            const auto* text = parsed->find("text");
            REQUIRE(text);
            REQUIRE(text->as_string());
            REQUIRE(mapper.begin(writer, loa::contracts::Prompt{*text->as_string(), {}, "owner"}));
        } else if (control && control->as_string() && *control->as_string() == "answer") {
            const auto* values = parsed->find("values");
            REQUIRE(values);
            REQUIRE(values->as_array());
            std::vector<std::string> answers;
            for (const auto& value : *values->as_array()) {
                REQUIRE(value.as_string());
                answers.push_back(*value.as_string());
            }
            NodeId decision;
            const Dom current = sink.dom();
            for (const auto& node : current.iter())
                if (node.kind == ElementKind::Decision && node.str_attr(attr::STATUS) == "waiting" && node.id.value > decision.value)
                    decision = node.id;
            REQUIRE(decision.value != 0);
            auto reply = mapper.answer(decision, loa::contracts::Answer{std::move(answers), std::nullopt, "owner"}, writer);
            REQUIRE_MESSAGE(reply, fixture.string(), ": ", line);
            if (*reply) replies.push_back(std::move(**reply));
        } else {
            auto decoded = wire::decode_input(*parsed);
            REQUIRE_MESSAGE(decoded, fixture.string(), ": ", line);
            auto mapped = mapper.apply(std::move(*decoded), writer);
            REQUIRE_MESSAGE(mapped, fixture.string(), ": ", line);
            replies.insert(replies.end(), std::make_move_iterator(mapped->begin()), std::make_move_iterator(mapped->end()));
        }
    }
    std::string actual = render(sink.dom()) + "\n--- replies\n";
    for (const auto& reply : replies) actual += json::dump(wire::to_json(reply)) + "\n";
    const auto expected = fixture.parent_path() / (fixture.stem().string() + ".golden");
    CHECK_MESSAGE(actual == read_file(expected), expected.string());
}

loa::contracts::SessionConfig config(const fs::path& cwd) {
    loa::contracts::SessionConfig cfg;
    cfg.session_id = "s";
    cfg.node = "owner";
    cfg.cwd = cwd;
    return cfg;
}

} // namespace

TEST_CASE("omp2: all transcript fixtures replay to their goldens") {
    const auto dir = golden("harness/omp2");
    REQUIRE(fs::is_directory(dir));
    std::vector<fs::path> fixtures;
    for (const auto& file : fs::directory_iterator(dir))
        if (file.path().extension() == ".jsonl") fixtures.push_back(file.path());
    std::sort(fixtures.begin(), fixtures.end());
    REQUIRE_FALSE(fixtures.empty());
    for (const auto& fixture : fixtures) {
        INFO(fixture.string());
        replay_fixture(fixture);
    }
}

TEST_CASE("omp2: approval choices honor advertised scopes and unsupported choices fall back to once") {
    MemorySink sink;
    const auto root = sink.commit_at(Draft::insert(NodeId::DOCUMENT, ElementKind::Session,
                                                    Attrs{{"id", "s"}, {"harness", "omp2"}, {"node", "owner"}, {"created", std::int64_t{1}}}, ""), 0);
    REQUIRE(root);
    Omp2Mapper mapper(root->node);
    const DomWriter writer = DomWriter(sink).at(0);
    REQUIRE(mapper.begin(writer, loa::contracts::Prompt{"run", {}, "owner"}));
    for (const auto& frame : {
             R"({"type":"tool_approval_request","promptId":"p1","title":"Run bash?","scopes":["once","session"]})",
             R"({"type":"tool_approval_request","promptId":"p2","title":"Run rm?","scopes":["once"]})",
             R"({"type":"tool_approval_request","promptId":"p3","title":"Run grep?"})"}) {
        auto input = wire::decode_input(*json::parse(frame));
        REQUIRE(input);
        REQUIRE(mapper.apply(std::move(*input), writer));
    }
    std::vector<NodeId> ids;
    const Dom dom = sink.dom();
    for (const auto& node : dom.iter()) {
        if (node.kind != ElementKind::Decision) continue;
        ids.push_back(node.id);
        const auto* options = node.attr(attr::OPTIONS);
        REQUIRE(options);
        const std::vector<std::string> expected = ids.size() == 2 ? std::vector<std::string>{"allow_once", "deny"}
                                                                   : std::vector<std::string>{"allow_once", "allow_session", "deny"};
        std::vector<std::string> actual;
        for (auto item : options->str_items()) actual.emplace_back(item);
        CHECK(actual == expected);
    }
    REQUIRE(ids.size() == 3);
    const auto session = mapper.answer(ids[0], loa::contracts::Answer{{"allow_session"}, std::nullopt, "owner"}, writer);
    REQUIRE(session);
    REQUIRE(*session);
    CHECK(*session == wire::Reply{wire::reply::Approve{"p1", true, "session", std::nullopt}});
    const auto fallback = mapper.answer(ids[1], loa::contracts::Answer{{"allow_session"}, std::nullopt, "owner"}, writer);
    REQUIRE(fallback);
    REQUIRE(*fallback);
    CHECK(*fallback == wire::Reply{wire::reply::Approve{"p2", true, "once", std::nullopt}});
}

TEST_CASE("omp2: a refused DOM subscription marks availability incompatible") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto fake = FakeProcess::install(dir.path(),
        "< set_dom_subscription sub\n"
        "> {\"type\":\"response\",\"id\":\"{{sub}}\",\"success\":false,\"error\":\"DOM unavailable\"}\n").value();
    REQUIRE(fake.set_version("2.0.0"));
    auto adapter = Omp2Adapter::with_executable(fake.executable());
    const auto found = block_on(exec, adapter.availability());
    CHECK(found.status == loa::contracts::AvailabilityStatus::Incompatible);
    REQUIRE(found.detail);
    CHECK(found.detail->find("DOM unavailable") != std::string::npos);
    CHECK(found.version == "2.0.0");
    REQUIRE(found.executable);
    CHECK(*found.executable == fake.executable());
    const auto received = fake.received();
    REQUIRE_FALSE(received.empty());
    CHECK(*received[0].find("type")->as_string() == "set_dom_subscription");
    CHECK(*received[0].find("enabled")->as_bool());
}

TEST_CASE("omp2: unsupported configuration is refused before writing to the sink") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    Omp2Adapter adapter = Omp2Adapter::with_executable(dir.path() / "unused");
    auto sink = std::make_shared<MemorySink>();
    auto cfg = config(dir.path());
    cfg.context = "project context";
    auto refused = block_on(exec, adapter.start(cfg, sink));
    REQUIRE_FALSE(refused);
    const auto* error = refused.error().get<loa::contracts::AdapterError::Unsupported>();
    REQUIRE(error);
    CHECK(error->harness == "omp2");
    CHECK(error->what == "injecting project context over omp2 RPC");
    CHECK(sink->dom().root() == nullptr);
}

TEST_CASE("omp2: session RPC calls use exact command names and parameters") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto fake = FakeProcess::install(dir.path(),
        "< set_dom_subscription sub\n"
        "> {\"type\":\"response\",\"id\":\"{{sub}}\",\"success\":true,\"data\":{}}\n"
        "< get_state state\n"
        "> {\"type\":\"response\",\"id\":\"{{state}}\",\"success\":true,\"data\":{\"sessionId\":\"native-1\"}}\n"
        "< get_available_models models\n"
        "> {\"type\":\"response\",\"id\":\"{{models}}\",\"success\":true,\"data\":{\"models\":[{\"provider\":\"acme\",\"id\":\"smart\",\"name\":\"Smart\",\"reasoning\":true},{\"provider\":\"acme\",\"id\":\"basic\",\"reasoning\":false}]}}\n"
        "< set_model model\n"
        "> {\"type\":\"response\",\"id\":\"{{model}}\",\"success\":true,\"data\":{}}\n"
        "< set_thinking_level level\n"
        "> {\"type\":\"response\",\"id\":\"{{level}}\",\"success\":true,\"data\":{}}\n"
        "< compact compact\n"
        "> {\"type\":\"response\",\"id\":\"{{compact}}\",\"success\":true,\"data\":{}}\n"
        "< prompt prompt\n"
        "> {\"type\":\"response\",\"id\":\"{{prompt}}\",\"success\":true,\"data\":{}}\n"
        "< abort abort\n"
        "> {\"type\":\"response\",\"id\":\"{{abort}}\",\"success\":true,\"data\":{}}\n"
        "> {\"type\":\"agent_end\",\"cancelled\":true}\n").value();
    auto sink = std::make_shared<MemorySink>();
    auto adapter = Omp2Adapter::with_executable(fake.executable());
    auto session = block_on(exec, adapter.start(config(dir.path()), sink));
    REQUIRE(session);
    CHECK((*session)->harness() == "omp2");
    CHECK((*session)->state() == loa::contracts::SessionState::Idle);
    REQUIRE(sink->dom().root());
    CHECK(sink->dom().root()->str_attr(attr::NATIVE_ID) == "native-1");
    auto listed = block_on(exec, (*session)->models());
    REQUIRE(listed);
    REQUIRE(listed->size() == 2);
    CHECK((*listed)[0].id == "acme/smart");
    CHECK((*listed)[0].name == "Smart");
    CHECK((*listed)[0].efforts == std::vector<std::string>{"off", "minimal", "low", "medium", "high", "xhigh"});
    CHECK((*listed)[1].efforts.empty());
    auto role = block_on(exec, (*session)->set_model({"acme/smart", "default"}));
    REQUIRE_FALSE(role);
    REQUIRE(role.error().get<loa::contracts::AdapterError::Unsupported>());
    CHECK(role.error().get<loa::contracts::AdapterError::Unsupported>()->what == "setting a model role over RPC");
    auto malformed = block_on(exec, (*session)->set_model({"smart", std::nullopt}));
    REQUIRE_FALSE(malformed);
    REQUIRE(malformed.error().get<loa::contracts::AdapterError::Protocol>());
    CHECK(malformed.error().get<loa::contracts::AdapterError::Protocol>()->detail == "omp2 models are `provider/model`, got `smart`");
    REQUIRE(block_on(exec, (*session)->set_model({"acme/smart", std::nullopt})));
    REQUIRE(block_on(exec, (*session)->set_effort("high")));
    REQUIRE(block_on(exec, (*session)->compact("short")));
    CHECK(sink->dom().root()->str_attr(attr::MODEL) == "acme/smart");
    CHECK(sink->dom().root()->str_attr(attr::EFFORT) == "high");
    REQUIRE(block_on(exec, (*session)->send({"hello", {}, "owner"})));
    CHECK((*session)->state() == loa::contracts::SessionState::Running);
    auto busy = block_on(exec, (*session)->send({"again", {}, "owner"}));
    REQUIRE_FALSE(busy);
    CHECK(busy.error().get<loa::contracts::AdapterError::Busy>());
    REQUIRE(block_on(exec, (*session)->interrupt()));
    REQUIRE(block_on(exec, (*session)->wait_idle()));
    CHECK((*session)->state() == loa::contracts::SessionState::Idle);
    REQUIRE(block_on(exec, (*session)->shutdown()));
    CHECK((*session)->state() == loa::contracts::SessionState::Closed);
    const auto received = fake.received();
    REQUIRE(received.size() >= 8);
    CHECK(*received[0].find("enabled")->as_bool());
    CHECK(*received[3].find("provider")->as_string() == "acme");
    CHECK(*received[3].find("modelId")->as_string() == "smart");
    CHECK(*received[4].find("level")->as_string() == "high");
    CHECK(*received[5].find("customInstructions")->as_string() == "short");
    CHECK(*received[6].find("message")->as_string() == "hello");
    CHECK(*received[7].find("type")->as_string() == "abort");
}

TEST_CASE("omp2: answering an approval sends the selected scope over RPC") {
    loa::runtime::Executor exec(2);
    auto dir = loa::runtime::TempDir::create().value();
    auto fake = FakeProcess::install(dir.path(),
        "< set_dom_subscription sub\n"
        "> {\"type\":\"response\",\"id\":\"{{sub}}\",\"success\":true,\"data\":{}}\n"
        "< get_state state\n"
        "> {\"type\":\"response\",\"id\":\"{{state}}\",\"success\":true,\"data\":{}}\n"
        "< prompt prompt\n"
        "> {\"type\":\"response\",\"id\":\"{{prompt}}\",\"success\":true,\"data\":{}}\n"
        "> {\"type\":\"tool_approval_request\",\"promptId\":\"p1\",\"title\":\"Run?\",\"scopes\":[\"once\",\"session\"]}\n"
        "< approve approve\n"
        "> {\"type\":\"response\",\"id\":\"{{approve}}\",\"success\":true,\"data\":{}}\n"
        "> {\"type\":\"agent_end\",\"cancelled\":false}\n").value();
    auto sink = std::make_shared<WatchSink>();
    auto adapter = Omp2Adapter::with_executable(fake.executable());
    auto session = block_on(exec, adapter.start(config(dir.path()), sink));
    REQUIRE(session);
    REQUIRE(block_on(exec, (*session)->send({"run", {}, "owner"})));
    REQUIRE(sink->until("approval request", [](const Dom& dom) {
        for (const auto& node : dom.iter())
            if (node.kind == ElementKind::Decision && node.str_attr(attr::STATUS) == "waiting") return true;
        return false;
    }));
    NodeId decision;
    const Dom dom = sink->inner.dom();
    for (const auto& node : dom.iter())
        if (node.kind == ElementKind::Decision) decision = node.id;
    REQUIRE(decision.value != 0);
    REQUIRE(block_on(exec, (*session)->answer(decision, {{"allow_session"}, std::nullopt, "owner"})));
    REQUIRE(block_on(exec, (*session)->wait_idle()));
    REQUIRE(block_on(exec, (*session)->shutdown()));
    const auto received = fake.received();
    REQUIRE(received.size() >= 4);
    CHECK(*received[3].find("type")->as_string() == "approve");
    CHECK(*received[3].find("promptId")->as_string() == "p1");
    CHECK(*received[3].find("approved")->as_bool());
    CHECK(*received[3].find("scope")->as_string() == "session");
    CHECK(sink->inner.dom().get(decision)->str_attr(attr::STATUS) == "answered");
}
