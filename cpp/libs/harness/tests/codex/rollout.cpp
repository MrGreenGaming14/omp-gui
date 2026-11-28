#include <doctest/doctest.h>

#include <filesystem>
#include <fstream>
#include <sstream>
#include <string>

#include "loa/core/sink.hpp"
#include "loa/core/writer.hpp"
#include "loa/harness/codex/rollout.hpp"
#include "loa/runtime/json.hpp"

namespace rollout = loa::harness::codex::rollout;
namespace core = loa::core;
namespace json = loa::runtime::json;

TEST_CASE("Codex rollout: fixture imports every native shape and exports byte-identically") {
    const auto fixture = std::filesystem::path(LOA_HARNESS_FIXTURES) / "codex/rollout-basic.jsonl";
    std::ifstream file(fixture, std::ios::binary);
    REQUIRE(file.good());
    const std::string original(std::istreambuf_iterator<char>{file}, {});
    std::istringstream input(original);
    core::MemorySink sink;
    auto imported = rollout::CodexNative::import(input, sink, "local-node");
    REQUIRE(imported.has_value());
    const auto dom = sink.dom();
    REQUIRE(dom.root());
    CHECK(dom.root()->str_attr(core::attr::NATIVE_ID) == "0000aaaa-1111-7222-8333-444455556666");
    bool compaction = false, shell = false, extension = false, usage = false;
    for (const auto& node : dom.iter()) {
        compaction |= node.kind == core::ElementKind::Compaction && node.text.find("three") != std::string::npos;
        shell |= node.kind == core::ElementKind::Call && node.str_attr(core::attr::TOOL) == "local_shell";
        extension |= node.kind == core::ElementKind::Ext && node.text.find("some_future_line") != std::string::npos;
        usage |= node.kind == core::ElementKind::Usage && node.int_attr(core::attr::INPUT_TOKENS) == 1200;
    }
    CHECK(compaction); CHECK(shell); CHECK(extension); CHECK(usage);
    std::ostringstream output;
    REQUIRE(rollout::CodexNative::export_(dom, output).has_value());
    CHECK(output.str() == original);
}

TEST_CASE("Codex rollout: editing message text keeps the original non-text fields") {
    std::istringstream input(R"({"timestamp":"2026-02-10T09:00:00.000Z","type":"session_meta","payload":{"id":"one","cwd":"/tmp"}}
{"timestamp":"2026-02-10T09:00:01.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"old","annotation":42}]}}
)");
    core::MemorySink sink;
    REQUIRE(rollout::import(input, sink, "owner").has_value());
    auto dom = sink.dom();
    for (const auto& node : dom.iter()) if (node.kind == core::ElementKind::Message) {
        core::DomWriter writer(sink);
        REQUIRE(writer.set_text(node.id, "new").has_value());
    }
    dom = sink.dom();
    std::ostringstream out;
    REQUIRE(rollout::export_(dom, out).has_value());
    auto pos = out.str().find('\n');
    REQUIRE(pos != std::string::npos);
    auto parsed = json::parse(std::string_view(out.str()).substr(pos + 1));
    REQUIRE(parsed.has_value());
    CHECK(*parsed->find("payload")->find("content")->as_array()->front().find("text")->as_string() == "new");
    CHECK(parsed->find("payload")->find("content")->as_array()->front().find("annotation")->as_i64() == 42);
}

TEST_CASE("Codex rollout: rejects absent session metadata and malformed JSON") {
    core::MemorySink sink;
    std::istringstream empty;
    auto missing = rollout::import(empty, sink, "node");
    REQUIRE_FALSE(missing.has_value());
    CHECK(missing.error().message().find("session_meta") != std::string::npos);
    std::istringstream bad("{not json}\n");
    auto invalid = rollout::import(bad, sink, "node");
    REQUIRE_FALSE(invalid.has_value());
    CHECK(invalid.error().message().find("line 1") != std::string::npos);
}
