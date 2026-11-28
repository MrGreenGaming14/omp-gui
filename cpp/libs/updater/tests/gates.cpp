#include <doctest/doctest.h>

#include <algorithm>
#include <filesystem>
#include <memory>

#include "loa/updater/gates.hpp"

using namespace loa::updater;
using loa::settings::Verdict;

namespace {

Json j(std::string_view text) { return *loa::runtime::json::parse(text); }
Json& member(Json& object, std::string_view key) {
    for (auto& [name, value] : *object.as_object())
        if (name == key) return value;
    throw std::logic_error("missing test field");
}

std::vector<ToolSchema> tools(Json edit) {
    return {{"edit", std::move(edit)}, {"notebook", j(R"({"type":"object","properties":{"cell":{"type":"integer"}}})")}};
}
Json old_edit() {
    return j(R"({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"},"edits":{"type":"array","items":{"type":"object","properties":{"line":{"type":"integer"}},"required":["line"]}}},"required":["path"]})");
}
UsedTools used() { return {{"edit", {"path", "content", "edits.line"}}}; }
Version v(std::string_view s) { return *Version::parse(s); }

} // namespace

TEST_CASE("tool schema diff matches the Rust gate") {
    struct Case {
        const char* name;
        void (*change)(std::vector<ToolSchema>&);
        Verdict verdict;
        std::vector<std::string> details;
    };
    const std::vector<Case> cases{
        {"unchanged", [](auto&) {}, Verdict::Pass, {}},
        {"used tool removed", [](auto& t) { t.erase(t.begin()); }, Verdict::Harmful, {"- edit: tool removed"}},
        {"unused tool removed", [](auto& t) { t.pop_back(); }, Verdict::Pass, {"- notebook: tool removed (unused)"}},
        {"field removed", [](auto& t) {
             auto& props = *member(t[0].input_schema, "properties").as_object();
             std::erase_if(props, [](const auto& p) { return p.first == "content"; });
         }, Verdict::Harmful, {"- edit.content: field removed"}},
        {"field renamed", [](auto& t) {
             auto& props = *member(t[0].input_schema, "properties").as_object();
             const Json path = *t[0].input_schema.find("properties")->find("path");
             std::erase_if(props, [](const auto& p) { return p.first == "path"; });
             props.emplace_back("file_path", path);
             t[0].input_schema.set("required", j(R"(["file_path"])"));
         }, Verdict::Harmful, {"- edit.path: field removed, likely renamed path -> file_path", "~ edit.file_path: now required"}},
        {"type changed", [](auto& t) {
             member(member(t[0].input_schema, "properties"), "content").set("type", "array");
         }, Verdict::Harmful, {"~ edit.content: type \"string\" -> \"array\""}},
        {"newly required", [](auto& t) {
             member(t[0].input_schema, "properties").set("mode", j(R"({"type":"string"})"));
             t[0].input_schema.set("required", j(R"(["path","mode"])"));
         }, Verdict::Harmful, {"~ edit.mode: now required"}},
        {"nested changes", [](auto& t) {
             auto& item = member(member(t[0].input_schema, "properties"), "edits");
             auto& nested = member(item, "items");
             member(nested, "properties").set("line", j(R"({"type":"string"})"));
             member(nested, "properties").set("end", j(R"({"type":"integer"})"));
             nested.set("required", j(R"(["line","end"])"));
         }, Verdict::Harmful, {"~ edit.edits.line: type \"integer\" -> \"string\"", "~ edit.edits.end: now required"}},
        {"additions", [](auto& t) {
             member(t[0].input_schema, "properties").set("dry_run", j(R"({"type":"boolean"})"));
             t.push_back({"search", j(R"({"type":"object"})")});
         }, Verdict::Pass, {"+ edit.dry_run: field added", "+ search: tool added"}},
        {"unused tool changed", [](auto& t) {
             t[1].input_schema = j(R"({"type":"object","properties":{"cell":{"type":"string"},"k":{}},"required":["k"]})");
         }, Verdict::Pass, {"+ notebook.k: field added (required)"}},
    };
    for (const auto& c : cases) {
        auto next = tools(old_edit());
        c.change(next);
        const auto result = check_tool_schemas(tools(old_edit()), next, used());
        INFO(c.name);
        CHECK(result.verdict == c.verdict);
        CHECK(result.details == c.details);
    }
    auto only_notebook = tools(old_edit());
    only_notebook.erase(only_notebook.begin());
    CHECK(check_tool_schemas(only_notebook, only_notebook, used()).details == std::vector<std::string>{"~ edit: unavailable in both versions"});
}

TEST_CASE("format config rpc contract and changelog checks") {
    const std::set<std::pair<std::uint32_t, std::uint32_t>> migrations{{1, 2}, {2, 3}};
    CHECK(check_session_format(1, 3, migrations).details == std::vector<std::string>{"~ 1 -> 3: migrated via 1 -> 2 -> 3"});
    CHECK(check_session_format(3, 1, migrations).verdict == Verdict::Harmful);
    ConfigSchema schema{{"a"}, {"roles"}, {{"b", "c"}}};
    CHECK(check_config_schema({"a", "roles.smol", "b", "d"}, schema).details == std::vector<std::string>{"- b: renamed to c", "- d: removed"});
    const auto rpc = check_rpc_protocol({1, 2}, {1, 3});
    CHECK(std::ranges::find(rpc.details, "negotiated: 2") != rpc.details.end());
    CHECK(check_rpc_protocol({3}, {1, 2}).verdict == Verdict::Harmful);
    CHECK(check_contract({{"basic: leaf", "- a\n+ b"}}).details == std::vector<std::string>{"basic: leaf:", "  - a", "  + b"});
    CHECK(check_changelog({{}, {"drop `grep`"}}).details == std::vector<std::string>{"- removed: drop `grep`"});
    CHECK(check_changelog({}).verdict == Verdict::Pass);
    CHECK(changelog_text({{v("1.10.0"), "ten"}, {v("1.2.0"), "two\n"}}) == "## 1.2.0\n\ntwo\n\n## 1.10.0\n\nten\n");
}

TEST_CASE("reviews parse strictly") {
    CHECK(parse_review("  {\"breaking\": [\"a\"], \"removed\": []}\n")->breaking == std::vector<std::string>{"a"});
    CHECK(parse_review("```json\n{\"breaking\": [], \"removed\": [\"b\"]}\n```")->removed == std::vector<std::string>{"b"});
    for (const auto* junk : {"{\"breaking\": [], \"removed\": [], \"notes\": []}", "{\"breaking\": []}",
                             "Here: {\"breaking\": [], \"removed\": []}", "[[], []]", "{\"breaking\": [1], \"removed\": []}"})
        CHECK_FALSE(parse_review(junk).has_value());
}

struct Fake final : Probe, ChangelogSummarizer {
    bool tools_fail = false;
    PortResult<std::vector<ToolSchema>> tools(const std::filesystem::path&) override {
        if (tools_fail) return std::unexpected(std::string("rpc client crashed"));
        return ::tools(old_edit());
    }
    PortResult<std::uint32_t> session_version(const std::filesystem::path& exe) override { return exe == "new" ? 2u : 1u; }
    PortResult<ConfigSchema> config_keys(const std::filesystem::path&) override { return ConfigSchema{OmpGates::defaults().written_keys, {}, {}}; }
    PortResult<std::vector<std::uint32_t>> protocol_versions(const std::filesystem::path&) override { return std::vector<std::uint32_t>{1, 2}; }
    PortResult<std::vector<ContractFailure>> run(const std::filesystem::path&) override { return std::vector<ContractFailure>{}; }
    PortResult<ChangelogReview> review(const std::string&) override { return ChangelogReview{}; }
};

TEST_CASE("run_gates runs every check and a failing port fails only its own") {
    auto gates = OmpGates::defaults();
    gates.used_tools = used();
    for (bool tools_fail : {false, true}) {
        Fake fake;
        fake.tools_fail = tools_fail;
        const auto report = run_gates("omp", v("1.0.0"), v("2.0.0"), GateInputs{"old", "new", &fake, &gates, &fake, "- fix"});
        std::vector<GateCheck> checks, harmful;
        for (const auto& c : report.checks) {
            checks.push_back(c.check);
            if (c.verdict == Verdict::Harmful) harmful.push_back(c.check);
        }
        CHECK(checks == std::vector<GateCheck>{GateCheck::ToolSchemas, GateCheck::SessionFormat, GateCheck::ConfigSchema,
                                               GateCheck::RpcProtocol, GateCheck::ContractTests, GateCheck::Changelog});
        CHECK(harmful == (tools_fail ? std::vector<GateCheck>{GateCheck::ToolSchemas} : std::vector<GateCheck>{}));
        if (tools_fail) CHECK(report.checks[0].details[0].find("current version: rpc client crashed") != std::string::npos);
    }
}
