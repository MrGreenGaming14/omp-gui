#include <doctest/doctest.h>

#include <algorithm>
#include <filesystem>
#include <sstream>

#include "loa/core/sink.hpp"
#include "loa/core/xml.hpp"
#include "loa/harness/opencode/ids.hpp"
#include "loa/harness/opencode/mapper.hpp"
#include "loa/harness/testing/testing.hpp"
#include "loa/runtime/json.hpp"

namespace {
namespace fs = std::filesystem;
namespace oc = loa::harness::opencode;
using loa::runtime::json::Object;
using loa::runtime::json::Value;

std::vector<fs::path> fixtures(std::string_view ext) {
    std::vector<fs::path> files;
    for (const auto& e : fs::directory_iterator(fs::path(LOA_HARNESS_FIXTURES) / "opencode"))
        if (e.path().extension() == ext) files.push_back(e.path());
    std::ranges::sort(files);
    return files;
}
} // namespace

TEST_CASE("opencode ids ascend within one millisecond") {
    const auto a = oc::ids::ascending("msg", 1'700'000'000'000);
    const auto b = oc::ids::ascending("msg", 1'700'000'000'000);
    CHECK(a.starts_with("msg_"));
    CHECK(a.size() == 4 + 26);
    CHECK(a.substr(0, 16) < b.substr(0, 16));
    // The hex prefix is timestamp_ms * 0x1000 + counter, as in Rust.
    CHECK(oc::ids::ascending("prt", 1).substr(0, 16) < oc::ids::ascending("prt", 2).substr(0, 16));
    CHECK(oc::ids::ascending("prt", 0x10).substr(4, 12) == "000000010001");
}

TEST_CASE("opencode transcripts replay to their goldens") {
    const auto files = fixtures(".jsonl");
    REQUIRE_FALSE(files.empty());
    for (const auto& fixture : files) {
        CAPTURE(fixture.string());
        loa::core::MemorySink sink;
        oc::RootInfo root;
        root.session_id = "s";
        root.node = "owner";
        root.native = Value(Object{{"id", "ses_1"}, {"time", Object{{"created", 1}}}});
        auto mapper = oc::Mapper::start(loa::core::DomWriter(sink).at(0), std::move(root));
        REQUIRE(mapper.has_value());
        auto decode = [](std::string_view line) -> std::expected<oc::Event, std::string> {
            auto value = loa::runtime::json::parse(line);
            if (!value) return std::unexpected(value.error().message());
            auto event = oc::parse_event(*value);
            if (!event) return std::unexpected(std::string("not an event"));
            return std::move(*event);
        };
        auto replies = loa::harness::testing::replay(*mapper, sink, fixture, decode);
        REQUIRE_MESSAGE(replies.has_value(), (replies ? std::string() : replies.error()));
        CHECK(replies->empty());
        std::istringstream xml(loa::core::xml::to_string(sink.dom()));
        std::string out, line;
        while (std::getline(xml, line))
            if (!line.starts_with("<!--")) out += line + "\n";
        auto same = loa::harness::testing::golden(fs::path(fixture).replace_extension(".golden"), out);
        CHECK_MESSAGE(same.has_value(), (same ? std::string() : same.error()));
    }
}
