#include <doctest/doctest.h>

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>

#include "loa/core/schema.hpp"
#include "loa/core/sink.hpp"
#include "loa/harness/omp/native.hpp"
#include "loa/harness/testing/testing.hpp"
#include "loa/runtime/tempfile.hpp"

namespace fs = std::filesystem;
namespace omp_native = loa::harness::omp::native;
namespace core = loa::core;

namespace {

std::string read_file(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}

} // namespace

TEST_CASE("omp native fixture sessions round trip") {
    const fs::path fixtures = fs::path(LOA_HARNESS_FIXTURES) / "omp/native";
    omp_native::OmpNative codec;
    for (const auto& file : fs::directory_iterator(fixtures)) {
        if (file.path().extension() != ".jsonl") continue;
        CAPTURE(file.path().string());
        core::MemorySink sink;
        auto imported = codec.import(file.path(), sink);
        REQUIRE(imported.has_value());
        const auto dom = sink.dom();
        auto exported = omp_native::export_string(dom);
        REQUIRE(exported.has_value());
        // Old session files migrate to version 3, so their normalized output is not verbatim.
        if (file.path().filename() == "v1.jsonl") {
            core::MemorySink migrated;
            REQUIRE(omp_native::import_str(*exported, migrated, "").has_value());
            auto stable = omp_native::export_string(migrated.dom());
            REQUIRE(stable.has_value());
            CHECK(*stable == *exported);
        } else {
            CHECK(*exported == read_file(file.path()));
        }
        auto round_trip = loa::harness::testing::round_trip(codec, file.path());
        CHECK_MESSAGE(round_trip.has_value(), (round_trip.has_value() ? std::string() : round_trip.error()));
    }
}

TEST_CASE("omp native import maps active messages and keeps branches") {
    constexpr std::string_view text =
        "{\"type\":\"session\",\"version\":3,\"id\":\"s\",\"timestamp\":\"2020-01-01T00:00:00Z\"}\n"
        "{\"type\":\"message\",\"id\":\"u\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n"
        "{\"type\":\"message\",\"id\":\"branch\",\"parentId\":\"u\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"old\"}]}}\n"
        "{\"type\":\"message\",\"id\":\"leaf\",\"parentId\":\"u\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"new\"}]}}\n";
    core::MemorySink sink;
    REQUIRE(omp_native::import_str(text, sink, "owner").has_value());
    const auto dom = sink.dom();
    REQUIRE(dom.root());
    CHECK(dom.root()->str_attr(core::attr::NODE) == "owner");
    const auto turns = dom.children_of_kind(dom.root()->id, core::ElementKind::Turn);
    REQUIRE(turns.size() == 1);
    const auto messages = dom.children_of_kind(turns.front()->id, core::ElementKind::Message);
    REQUIRE(messages.size() == 2);
    CHECK(messages.front()->text == "hello");
    CHECK(messages.back()->text == "new");
    const auto branches = dom.children_of_kind(dom.root()->id, core::ElementKind::Ext);
    REQUIRE(branches.size() == 1);
    CHECK(branches.front()->str_attr(core::attr::TYPE) == "branch");
    CHECK(branches.front()->text.find("\"branch\"") != std::string::npos);
    auto exported = omp_native::export_string(dom);
    REQUIRE(exported.has_value());
    CHECK(*exported == text);
}

TEST_CASE("omp native migrates version one and skips malformed lines") {
    constexpr std::string_view text =
        "garbage\n"
        "{\"type\":\"session\",\"version\":1,\"id\":\"old\",\"timestamp\":\"2020-01-01T00:00:00Z\"}\n"
        "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n"
        "{\"type\":\"message\",\"message\":{\"role\":\"hookMessage\",\"content\":\"custom\"}}\n";
    core::MemorySink sink;
    REQUIRE(omp_native::import_str(text, sink, "").has_value());
    auto exported = omp_native::export_string(sink.dom());
    REQUIRE(exported.has_value());
    CHECK(exported->find("\"version\":3") != std::string::npos);
    CHECK(exported->find("\"id\":\"00000001\"") != std::string::npos);
    CHECK(exported->find("\"role\":\"custom\"") != std::string::npos);
    CHECK(exported->find("garbage") == std::string::npos);
}

TEST_CASE("omp native import requires a session header") {
    core::MemorySink sink;
    auto missing = omp_native::import_str("{\"type\":\"title\",\"title\":\"Only\"}\n", sink, "");
    REQUIRE_FALSE(missing.has_value());
    CHECK(missing.error().get<loa::contracts::AdapterError::Format>() != nullptr);
    CHECK(sink.dom().empty());
}

TEST_CASE("omp native content text extracts text blocks") {
    auto parsed = loa::runtime::json::parse(R"([{"type":"thinking","thinking":"secret"},{"type":"text","text":"one"},{"type":"text","text":"two"}])");
    REQUIRE(parsed.has_value());
    CHECK(omp_native::content_text(&*parsed) == "one\ntwo");
    CHECK(omp_native::content_text(nullptr).empty());
}

TEST_CASE("omp native codec discovers project files titled by a leading title line") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp.has_value());
    const auto file = tmp->path() / ".omp/agent/sessions/project/session.jsonl";
    fs::create_directories(file.parent_path());
    std::ofstream(file, std::ios::binary)
        << "{\"type\":\"title\",\"title\":\"From title line\"}\n"
           "{\"type\":\"session\",\"version\":3,\"id\":\"s\"}\n";
    omp_native::OmpNative codec;
    auto found = codec.discover(tmp->path());
    REQUIRE(found.size() == 1);
    CHECK(found[0].path == file);
    CHECK(found[0].project == "project");
    CHECK(found[0].title == "From title line");
}
