#include <doctest/doctest.h>

#include <algorithm>

#include <filesystem>
#include <fstream>

#include "../kit/support.hpp"
#include "loa/core/sink.hpp"
#include "loa/core/writer.hpp"
#include "loa/harness/claude/native.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/tempfile.hpp"

namespace fs = std::filesystem;
namespace j = loa::runtime::json;
namespace native = loa::harness::claude::native;
using namespace loa::core;

namespace {
fs::path fixture() {
    return fs::path(LOA_HARNESS_FIXTURES) / "claude/native/11111111-2222-4333-8444-555555555555.jsonl";
}
void sort_json(j::Value& value) {
    if (auto* object = value.as_object()) {
        for (auto& [_, item] : *object) sort_json(item);
        std::ranges::sort(*object, {}, [](const auto& entry) -> const std::string& { return entry.first; });
    } else if (auto* array = value.as_array()) {
        for (auto& item : *array) sort_json(item);
    }
}
std::vector<j::Value> json_lines(const fs::path& path) {
    std::ifstream in(path);
    std::vector<j::Value> lines;
    std::string line;
    while (std::getline(in, line)) if (!line.empty()) {
        auto parsed = *j::parse(line);
        sort_json(parsed);
        lines.push_back(std::move(parsed));
    }
    return lines;
}
} // namespace

TEST_CASE("claude native: imported transcript and side files round-trip losslessly") {
    const auto source = fixture();
    REQUIRE(fs::exists(source));
    MemorySink sink;
    REQUIRE(native::import(source, sink).has_value());
    auto dom = sink.dom();
    REQUIRE(dom.root());
    CHECK(dom.root()->str_attr(attr::ID) == "11111111-2222-4333-8444-555555555555");
    CHECK(dom.root()->str_attr(attr::TITLE) == "Invented title");
    const Node* call = nullptr;
    const Node* job = nullptr;
    const Node* result = nullptr;
    const Node* compaction = nullptr;
    for (const auto& node : dom.iter()) {
        if (node.kind == ElementKind::Call) call = &node;
        if (node.kind == ElementKind::Job) job = &node;
        if (node.kind == ElementKind::Result) result = &node;
        if (node.kind == ElementKind::Compaction) compaction = &node;
    }
    REQUIRE(call);
    REQUIRE(job);
    REQUIRE(result);
    REQUIRE(compaction);
    CHECK(call->str_attr(attr::CALL_ID) == "toolu_1");
    CHECK(call->str_attr(attr::STATUS) == "done");
    CHECK(job->str_attr("x-claude-file") == "subagents/agent-sub-7.jsonl");
    CHECK(job->str_attr(attr::AGENT) == "Explore");
    CHECK(result->text == "first\nsecond");
    CHECK(compaction->int_attr(attr::TOKENS_BEFORE) == 321);

    auto temp = loa::runtime::TempDir::create().value();
    const auto out = temp.path() / "exported.jsonl";
    REQUIRE(native::export_(dom, out).has_value());
    CHECK(json_lines(out) == json_lines(source));
    auto source_side = source; source_side.replace_extension();
    for (auto name : {"subagents/agent-sub-7.jsonl", "subagents/agent-sub-7.meta.json", "tool-results/blob.bin"})
        CHECK(loa::harness::test::read_file(temp.path() / "exported" / name) == loa::harness::test::read_file(source_side / name));

    MemorySink child;
    REQUIRE(native::import(source_side / "subagents/agent-sub-7.jsonl", child).has_value());
    const auto child_out = temp.path() / "child.jsonl";
    REQUIRE(native::export_(child.dom(), child_out).has_value());
    CHECK(json_lines(child_out) == json_lines(source_side / "subagents/agent-sub-7.jsonl"));

    auto message = sink.with_dom([](const Dom& dom) {
        for (const auto& node : dom.iter()) if (node.text == "Checking file") return node.id;
        return NodeId::DOCUMENT;
    });
    REQUIRE(message != NodeId::DOCUMENT);
    REQUIRE(sink.commit(Draft::append(message, " again")).has_value());
    REQUIRE(native::export_(sink.dom(), out).has_value());
    const auto edited = json_lines(out);
    const auto* block = edited[2].find("message")->find("content")->as_array();
    REQUIRE(block);
    CHECK(*(*block)[1].find("text") == j::Value("Checking file again"));
    CHECK((*block)[1].find("citations") != nullptr);
    CHECK(edited[2].find("message")->find("usage")->find("unknown_usage")->as_i64() == 17);
}

TEST_CASE("claude native: fresh messages chain and side paths stay inside export folder") {
    MemorySink sink;
    DomWriter writer(sink);
    auto root = writer.insert(NodeId::DOCUMENT, ElementKind::Session,
        {{std::string(attr::ID), "s"}, {std::string(attr::HARNESS), "claude-code"}, {std::string(attr::NODE), "n"},
         {std::string(attr::CREATED), 0}, {std::string(attr::SCHEMA), 1}}).value();
    auto turn = writer.insert(root, ElementKind::Turn,
        {{std::string(attr::STATUS), "done"}, {std::string(attr::STARTED), 1000}}).value();
    REQUIRE(writer.insert(turn, ElementKind::Message,
        {{std::string(attr::ROLE), "user"}, {std::string(attr::STATUS), "done"}}, "Start here").has_value());
    REQUIRE(writer.insert(turn, ElementKind::Message,
        {{std::string(attr::ROLE), "assistant"}, {std::string(attr::STATUS), "done"}}, "Here is the result").has_value());
    auto temp = loa::runtime::TempDir::create().value();
    auto out = temp.path() / "fresh.jsonl";
    REQUIRE(native::export_(sink.dom(), out).has_value());
    const auto lines = json_lines(out);
    REQUIRE(lines.size() == 2);
    CHECK(lines[0].find("parentUuid")->is_null());
    CHECK(*lines[1].find("parentUuid") == *lines[0].find("uuid"));
    CHECK(*lines[0].find("message")->find("content") == j::Value("Start here"));
    CHECK(*lines[1].find("message")->find("content")->as_array()->at(0).find("text") == j::Value("Here is the result"));
    REQUIRE(writer.insert(root, ElementKind::Ext,
        {{std::string(attr::HARNESS), "claude-code"}, {std::string(attr::TYPE), "file"}, {"x-path", "../escape"}}, "data").has_value());
    auto rejected = native::export_(sink.dom(), out);
    CHECK_FALSE(rejected.has_value());
    REQUIRE_FALSE(fs::exists(temp.path() / "escape"));
}

TEST_CASE("claude native: fresh tool calls and results keep their ids and assistant source") {
    MemorySink sink;
    DomWriter w(sink);
    auto root = w.insert(NodeId::DOCUMENT, ElementKind::Session,
        {{std::string(attr::ID), "s"}, {std::string(attr::HARNESS), "claude-code"},
         {std::string(attr::NODE), ""}, {std::string(attr::CREATED), 0}, {std::string(attr::SCHEMA), 1}}).value();
    auto turn = w.insert(root, ElementKind::Turn,
        {{std::string(attr::STATUS), "done"}, {std::string(attr::STARTED), 1000}}).value();
    auto call = w.insert(turn, ElementKind::Call,
        {{std::string(attr::TOOL), "Read"}, {std::string(attr::CALL_ID), "toolu_test"},
         {std::string(attr::STATUS), "done"}, {std::string(attr::STARTED), 1000}, {std::string(attr::ENDED), 2000}}).value();
    REQUIRE(w.insert(call, ElementKind::Input, {{std::string(attr::FORMAT), "json"}}, R"({"file_path":"/tmp/demo"})").has_value());
    REQUIRE(w.insert(call, ElementKind::Result, {}, "read output").has_value());
    auto temp = loa::runtime::TempDir::create().value();
    auto out = temp.path() / "tools.jsonl";
    REQUIRE(native::export_(sink.dom(), out).has_value());
    auto lines = json_lines(out);
    REQUIRE(lines.size() == 2);
    CHECK(*lines[0].find("message")->find("content")->as_array()->at(0).find("id") == j::Value("toolu_test"));
    CHECK(*lines[1].find("message")->find("content")->as_array()->at(0).find("tool_use_id") == j::Value("toolu_test"));
    CHECK(*lines[1].find("sourceToolAssistantUUID") == *lines[0].find("uuid"));
    MemorySink imported;
    REQUIRE(native::import(out, imported).has_value());
    auto imported_dom = imported.dom();
    bool found = false;
    for (const auto& node : imported_dom.iter())
        if (node.kind == ElementKind::Result && node.text == "read output") found = true;
    CHECK(found);
}
