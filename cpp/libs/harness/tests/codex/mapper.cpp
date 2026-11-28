#include <doctest/doctest.h>

#include <filesystem>
#include <fstream>
#include <memory>
#include <sstream>

#include "loa/harness/codex/mapper.hpp"
#include "loa/harness/testing/testing.hpp"

namespace {
using namespace loa::core;
using namespace loa::harness::codex;
namespace json = loa::runtime::json;

std::string state_name(loa::contracts::SessionState state) {
    switch (state) {
    case loa::contracts::SessionState::Idle: return "Idle";
    case loa::contracts::SessionState::Running: return "Running";
    case loa::contracts::SessionState::Waiting: return "Waiting";
    case loa::contracts::SessionState::Closed: return "Closed";
    }
    return {};
}
void sort_json(Json& value) {
    if (auto* object = value.as_object()) {
        for (auto& [key, child] : *object) sort_json(child);
        std::sort(object->begin(), object->end(), [](const auto& a, const auto& b) { return a.first < b.first; });
    } else if (auto* list = value.as_array()) {
        for (auto& child : *list) sort_json(child);
    }
}
std::string canonical(Json value) { sort_json(value); return json::dump(value); }
void log_reply(std::string& log, const Reply& reply) {
    log += "> " + json::dump(reply.id);
    switch (reply.kind) {
    case Reply::Kind::Result: log += " " + canonical(reply.result); break;
    case Reply::Kind::Error: log += " error " + std::to_string(reply.code) + " " + reply.message; break;
    case Reply::Kind::HostTool: log += " host " + reply.tool + " " + reply.arguments; break;
    }
    log += '\n';
}
std::pair<std::string_view, std::string_view> split(std::string_view s, std::string_view separator) {
    auto at = s.find(separator);
    return at == s.npos ? std::pair{s, std::string_view{}} : std::pair{s.substr(0, at), s.substr(at + separator.size())};
}
} // namespace

TEST_CASE("codex: mapper transcript matches Rust golden") {
    const auto fixtures = std::filesystem::path(LOA_HARNESS_FIXTURES) / "codex";
    std::ifstream stream(fixtures / "turns.jsonl");
    REQUIRE(stream.good());
    loa::contracts::SessionConfig config;
    config.session_id = "session-invented";
    config.node = "local-node";
    config.cwd = "/work/invented";
    config.model = "gpt-invented";
    auto sink = std::make_unique<MemorySink>();
    auto started = Mapper::start(config, "thread-one", "gpt-invented", DomWriter(*sink));
    REQUIRE(started.has_value());
    Mapper mapper = std::move(*started);
    std::string log;
    std::string line;
    while (std::getline(stream, line)) {
        if (line.empty() || line.starts_with('#')) continue;
        auto w = DomWriter(*sink);
        loa::contracts::Result<std::optional<Reply>> outcome = std::optional<Reply>{};
        if (line.starts_with("! ")) {
            auto [verb, rest] = split(std::string_view(line).substr(2), " ");
            loa::contracts::Result<> action;
            if (verb == "prompt") action = mapper.begin_turn({std::string(rest), {}, "person"}, w);
            else if (verb == "accepted") action = mapper.turn_accepted(rest, w);
            else if (verb == "rejected") action = mapper.turn_rejected(rest, w);
            else if (verb == "model") action = mapper.set_model(rest, w);
            else if (verb == "effort") action = mapper.set_effort(rest, w);
            else if (verb == "mode") {
                auto mode = parse<SessionMode>(rest);
                REQUIRE(mode);
                action = mapper.set_mode(*mode, w);
            } else if (verb == "exited") action = mapper.process_exited(rest, w);
            else if (verb == "attach") {
                auto dom = sink->dom();
                sink = std::make_unique<MemorySink>(dom);
                auto attached = Mapper::attach(dom, DomWriter(*sink));
                if (attached) mapper = std::move(*attached);
                else action = std::unexpected(attached.error());
            } else if (verb == "state") log += "= " + state_name(mapper.state()) + "\n";
            else if (verb == "answer") {
                auto [ordinal, choices] = split(rest, " ");
                auto [values, reason] = split(choices, " | ");
                auto dom = sink->dom();
                std::vector<NodeId> decisions;
                for (const Node& node : dom.iter())
                    if (node.kind == ElementKind::Decision) decisions.push_back(node.id);
                std::size_t index = std::stoul(std::string(ordinal));
                REQUIRE(index < decisions.size());
                loa::contracts::Answer answer;
                answer.by = "person";
                if (!reason.empty()) answer.reason = reason;
                if (values.starts_with('{')) answer.values.emplace_back(values);
                else {
                    while (true) {
                        auto [value, remaining] = split(values, ",");
                        answer.values.emplace_back(value);
                        if (remaining.empty()) break;
                        values = remaining;
                    }
                }
                outcome = mapper.answer(decisions[index], answer, w);
            } else FAIL("unknown mapper transcript directive");
            if (verb != "answer" && !action) outcome = std::unexpected(action.error());
        } else {
            auto message = json::parse(line);
            REQUIRE(message);
            const auto* method = message->find("method");
            REQUIRE(method);
            REQUIRE(method->as_string());
            const auto* params = message->find("params");
            const auto* id = message->find("id");
            outcome = mapper.handle(id ? std::optional<Json>(*id) : std::nullopt, *method->as_string(),
                                    params ? *params : Json{}, w);
        }
        if (!outcome) log += "! error " + outcome.error().message() + "\n";
        else if (*outcome) log_reply(log, **outcome);
    }
    log += loa::harness::testing::render(sink->dom());
    auto golden = loa::harness::testing::golden(fixtures / "turns.golden", log);
    CHECK_MESSAGE(golden.has_value(), (golden.has_value() ? std::string() : golden.error()));
}
