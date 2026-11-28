// omp mapper: transcript replays against goldens, turn times, title requests and subagent
// progress (port of `tests/omp.rs`).
#include <doctest/doctest.h>

#include <algorithm>
#include <cstdio>
#include <filesystem>
#include <string>
#include <utility>
#include <vector>

#include "loa/core/sink.hpp"
#include "loa/core/writer.hpp"
#include "loa/core/xml.hpp"
#include "loa/harness/omp/mapper.hpp"
#include "loa/harness/testing/testing.hpp"
#include "loa/runtime/json.hpp"

using namespace loa::core;
using namespace loa::harness::omp;
namespace json = loa::runtime::json;
namespace fs = std::filesystem;

namespace {

std::vector<fs::path> fixtures() {
    std::vector<fs::path> files;
    for (const auto& entry : fs::directory_iterator(fs::path(LOA_HARNESS_FIXTURES) / "omp"))
        if (entry.path().extension() == ".jsonl") files.push_back(entry.path());
    std::ranges::sort(files);
    return files;
}

NodeId insert_session(PatchSink& sink) {
    Attrs attrs{{"id", "s"}, {"harness", "omp"}, {"node", "owner"}, {"created", std::int64_t{1}}, {"schema", std::int64_t{1}}};
    auto patch = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, std::move(attrs), ""));
    REQUIRE(patch);
    return patch->node;
}

// Rust's `{:?}` of a string.
std::string debug(std::string_view s) {
    std::string out = "\"";
    for (unsigned char c : s) {
        switch (c) {
        case '"': out += "\\\""; break;
        case '\\': out += "\\\\"; break;
        case '\n': out += "\\n"; break;
        case '\r': out += "\\r"; break;
        case '\t': out += "\\t"; break;
        default:
            if (c < 0x20 || c == 0x7f) {
                char buf[16];
                std::snprintf(buf, sizeof buf, "\\u{%x}", c);
                out += buf;
            } else {
                out.push_back(static_cast<char>(c));
            }
        }
    }
    return out + "\"";
}

std::string debug(const std::vector<std::string>& list) {
    std::string out = "[";
    for (std::size_t i = 0; i < list.size(); ++i) out += (i ? ", " : "") + debug(list[i]);
    return out + "]";
}

std::string debug(const rpc::UiResponse& response) {
    using namespace rpc::ui_response;
    if (const auto* v = std::get_if<rpc::ui_response::Value>(&response)) return "Value { id: " + debug(v->id) + ", value: " + debug(v->value) + " }";
    if (const auto* v = std::get_if<Confirmed>(&response))
        return "Confirmed { id: " + debug(v->id) + ", confirmed: " + (v->confirmed ? "true" : "false") + " }";
    if (const auto* v = std::get_if<Cancelled>(&response))
        return "Cancelled { id: " + debug(v->id) + ", timed_out: " + (v->timed_out ? "true" : "false") + " }";
    const auto& answers = std::get<Answers>(response);
    std::string out = "Answers { id: " + debug(answers.id) + ", answers: [";
    for (std::size_t i = 0; i < answers.answers.size(); ++i) {
        const auto& a = answers.answers[i];
        out += std::string(i ? ", " : "") + "AskAnswer { id: " + debug(a.id) + ", selected_options: " + debug(a.selected_options) +
               ", custom_input: " + (a.custom_input ? "Some(" + debug(*a.custom_input) + ")" : std::string("None")) + " }";
    }
    return out + "] }";
}

Input input(std::string_view line) {
    auto parsed = json::parse(line);
    REQUIRE_MESSAGE(parsed, line);
    return input_from_json(std::move(*parsed));
}

// A mapper over a fresh session, fed JSON lines at the writer's clock.
struct Fed {
    MemorySink sink;
    Mapper mapper;
    explicit Fed(bool titled = false) : mapper(insert_session(sink)) { mapper.titled(titled); }
    void operator()(std::string_view line) {
        auto replies = mapper.apply(input(line), DomWriter(sink));
        REQUIRE_MESSAGE(replies, line);
    }
};

using Pair = std::pair<bool, bool>;

constexpr std::string_view SEND = R"({"$": "send", "id": "p", "text": "read a.rs"})";
constexpr std::string_view START = R"({"type": "agent_start"})";

std::string result(std::string_view status) {
    return std::string(R"({"type": "prompt_result", "id": "p", "agentInvoked": true, "status": ")") + std::string(status) +
           R"(", "sessionSettled": true})";
}

} // namespace

TEST_CASE("omp adapter: transcripts replay to their goldens") {
    const auto files = fixtures();
    REQUIRE(files.size() >= 4);
    for (const auto& fixture : files) {
        CAPTURE(fixture.string());
        MemorySink sink;
        Mapper mapper(insert_session(sink));
        auto replies = loa::harness::testing::replay(mapper, sink, fixture, [](std::string_view line) -> std::expected<Input, std::string> {
            auto parsed = json::parse(line);
            if (!parsed) return std::unexpected(parsed.error().message());
            return input_from_json(std::move(*parsed));
        });
        REQUIRE_MESSAGE(replies, replies.error());
        // The seq comment is left out: goldens compare content only.
        const std::string xml = loa::core::xml::to_string(sink.dom());
        std::string out;
        std::size_t start = 0;
        while (start < xml.size()) {
            auto end = xml.find('\n', start);
            if (end == std::string::npos) end = xml.size();
            std::string_view line(xml.data() + start, end - start);
            if (!line.starts_with("<!--")) { out.append(line); out.push_back('\n'); }
            start = end + 1;
        }
        out += "\n--- replies\n";
        for (const auto& reply : *replies) {
            out += "ui " + debug(reply.response) + "\n";
            if (reply.steer) out += "steer " + debug(*reply.steer) + "\n";
        }
        auto golden = loa::harness::testing::golden(fs::path(fixture).replace_extension(".golden"), out);
        CHECK_MESSAGE(golden, (golden ? std::string() : golden.error()));
    }
}

TEST_CASE("omp adapter: a turn records when it started and when it ended") {
    Fed feed;
    feed(SEND);
    feed(START);
    auto times = [&] {
        const auto dom = feed.sink.dom();
        for (const auto& node : dom.iter())
            if (node.kind == ElementKind::Turn) return std::pair{node.int_attr(attr::STARTED), node.int_attr(attr::ENDED)};
        FAIL("no turn");
        return std::pair<std::optional<std::int64_t>, std::optional<std::int64_t>>{};
    };
    auto [started, ended] = times();
    CHECK(started.has_value());
    CHECK_FALSE(ended.has_value());
    feed(result("completed"));
    std::tie(started, ended) = times();
    CHECK(started.has_value());
    CHECK(ended.has_value());
}

TEST_CASE("omp adapter: only an untitled session whose turn completed asks for a title once") {
    auto run = [](bool titled, std::vector<std::string> lines) {
        Fed feed(titled);
        for (const auto& line : lines) feed(line);
        const bool first = feed.mapper.take_rename();
        return std::pair{first, feed.mapper.take_rename()};
    };
    auto turn = [](std::string_view status) { return std::vector<std::string>{std::string(SEND), std::string(START), result(status)}; };
    CHECK(run(false, turn("completed")) == (Pair{true, false}));
    CHECK(run(false, turn("aborted")) == (Pair{false, false}));
    CHECK(run(true, turn("completed")) == (Pair{false, false}));
    auto titled = turn("completed");
    titled.insert(titled.begin(), R"({"type": "session_info_update", "title": "Named", "sessionId": "n"})");
    CHECK(run(false, titled) == (Pair{false, false}));
    auto queued = turn("completed");
    queued.emplace_back(R"({"$": "send", "id": "q", "text": "next"})");
    CHECK_MESSAGE((run(false, queued) == Pair{false, false}), "waits until nothing runs");
}

TEST_CASE("omp adapter: subagent progress matches nested id and legacy id without fake rows") {
    Fed feed;
    for (std::string_view line : {
             R"({"$": "send", "id": "p1", "text": "run subagents"})",
             R"({"type": "agent_start"})",
             R"({"type": "turn_start"})",
             // A shell job whose id must never be targeted by subagent_progress.
             R"({"type": "tool_execution_start", "toolCallId": "bash-1", "toolName": "bash", "args": {"command": "echo ok"}})",
             R"({"type": "tool_execution_end", "toolCallId": "bash-1", "toolName": "bash", "result": "ok"})",
             // Concurrent subagent jobs sharing agent name and overlapping indices.
             R"({"type": "subagent_lifecycle", "payload": {"id": "sub-alpha", "agent": "explore", "description": "Survey alpha", "status": "started", "sessionFile": "/tmp/alpha.jsonl"}})",
             R"({"type": "subagent_lifecycle", "payload": {"id": "sub-beta", "agent": "explore", "description": "Survey beta", "status": "started", "sessionFile": "/tmp/beta.jsonl"}})",
             // omp 19.1.1 frames: nested progress.id with missing or mismatching parentToolCallId.
             R"({"type": "subagent_progress", "payload": {"index": 0, "agent": "explore", "task": "Survey beta", "progress": {"id": "sub-beta", "status": "running", "currentTool": "grep"}}})",
             R"({"type": "subagent_progress", "payload": {"index": 99, "agent": "explore", "task": "Survey alpha", "parentToolCallId": "wrong-parent", "progress": {"id": "sub-alpha", "status": "running", "currentTool": "read"}}})",
             // Exact agent:{parentToolCallId}:{index} fallback when nested progress.id is absent.
             R"({"type": "subagent_lifecycle", "payload": {"id": "agent:call-legacy:0", "agent": "reviewer", "description": "Legacy child", "status": "started"}})",
             R"({"type": "subagent_progress", "payload": {"index": 0, "agent": "reviewer", "parentToolCallId": "call-legacy", "progress": {"status": "running", "step": "legacy"}}})",
             // An unknown canonical id must not fall back even when the legacy id matches.
             R"({"type": "subagent_progress", "payload": {"index": 0, "agent": "explore", "parentToolCallId": "call-legacy", "progress": {"id": "missing-id", "step": "unmatched"}}})",
             // A missing index must not silently resolve to zero.
             R"({"type": "subagent_progress", "payload": {"agent": "reviewer", "parentToolCallId": "call-legacy", "progress": {"step": "missing-index"}}})",
             R"({"type": "subagent_progress", "payload": {"index": 0, "agent": "explore", "progress": {"id": "bash-1", "step": "shell-collision"}}})",
             R"({"type": "subagent_lifecycle", "payload": {"id": "sub-alpha", "agent": "explore", "status": "completed"}})",
             R"({"type": "subagent_lifecycle", "payload": {"id": "sub-beta", "agent": "explore", "status": "completed"}})",
             R"({"type": "subagent_lifecycle", "payload": {"id": "agent:call-legacy:0", "agent": "explore", "status": "completed"}})",
             R"({"type": "prompt_result", "id": "p1", "agentInvoked": true, "status": "completed", "sessionSettled": true})",
             R"({"type": "session_settled"})",
             // Late unmatched progress after turn end must not open a phantom turn or insert anything.
             R"({"type": "subagent_progress", "payload": {"index": 0, "agent": "explore", "progress": {"id": "sub-alpha", "step": "late"}}})",
         })
        feed(line);

    const auto dom = feed.sink.dom();
    CHECK(loa::harness::testing::check_dom(dom).empty());
    CHECK(loa::harness::testing::check_history(feed.sink.patches()).empty());
    auto count = [&](ElementKind kind) {
        return std::ranges::count_if(dom.iter(), [&](const auto& n) { return n.kind == kind; });
    };
    CHECK(count(ElementKind::Turn) == 1);
    CHECK(count(ElementKind::Job) == 4);
    CHECK_FALSE(std::ranges::any_of(dom.iter(), [](const auto& n) {
        return n.kind == ElementKind::Ext && n.str_attr(attr::TYPE) == std::optional<std::string_view>("subagent_progress");
    }));
    auto job_text = [&](std::string_view id) {
        for (const auto& n : dom.iter())
            if (n.kind == ElementKind::Job && n.str_attr(attr::ID) == std::optional<std::string_view>(id)) return n.text;
        FAIL("no job ", id);
        return std::string();
    };
    CHECK(job_text("sub-alpha").find(R"("currentTool":"read")") != std::string::npos);
    CHECK(job_text("sub-beta").find(R"("currentTool":"grep")") != std::string::npos);
    const auto legacy = job_text("agent:call-legacy:0");
    CHECK(legacy.find(R"("step":"legacy")") != std::string::npos);
    CHECK(legacy.find("unmatched") == std::string::npos);
    CHECK(legacy.find("missing-index") == std::string::npos);
    CHECK(job_text("bash-1").empty());
}
