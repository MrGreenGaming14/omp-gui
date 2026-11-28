#include "loa/app/adapters/update_gates.hpp"

#include <asio/awaitable.hpp>

#include <doctest/doctest.h>

#include <algorithm>
#include <array>
#include <fstream>
#include <iterator>
#include <sstream>

#include "loa/runtime/exec.hpp"
#include "loa/runtime/tempfile.hpp"

namespace {

namespace adapters = loa::app::adapters;
namespace detail = loa::app::adapters::detail;
namespace json = loa::runtime::json;
using detail::Json;

Json parse(std::string_view text) {
    auto v = json::parse(text);
    REQUIRE(v);
    return std::move(*v);
}

std::string read_file(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>()};
}

class Canned final : public loa::contracts::Summarizer {
  public:
    explicit Canned(std::string reply) : reply_(std::move(reply)) {}
    loa::contracts::Awaitable<std::expected<std::string, std::string>> summarize(std::string instructions,
                                                                                std::string input) const override {
        CHECK(instructions.ends_with("Changelog:\n\n"));
        CHECK(input.find("- fix") != std::string::npos);
        co_return reply_;
    }

  private:
    std::string reply_;
};

} // namespace

TEST_CASE("update_gates: refusals and config lists parse") {
    const auto unavailable = "Error: Built-in tool unavailable in this session: ast_grep.\nRun `omp --help`";
    CHECK(detail::refused_tool(unavailable) == std::optional<std::string>("ast_grep"));
    CHECK(detail::refused_tool("Error: Unknown tool in --tools: bogus.") == std::optional<std::string>("bogus"));
    CHECK(detail::refused_tool("Error: something else") == std::nullopt);
    const auto list = R"({"modelRoles":{"type":"record","value":{}},"memory.backend":{"type":"enum","value":"off"}})";
    auto schema = detail::parse_config_list(list);
    REQUIRE(schema);
    CHECK(schema->accepts("modelRoles.smol"));
    CHECK(schema->accepts("memory.backend"));
    CHECK_FALSE(schema->accepts("memory.backend.x"));
    CHECK_FALSE(detail::parse_config_list("[]"));
}

TEST_CASE("update_gates: ready probe reads offered versions in and out of range") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    adapters::OmpProbe probe({}, std::chrono::seconds(20));
    const std::array<std::string, 2> all{"[1,2]", "[3]"};
    for (std::size_t i = 0; i < all.size(); ++i) {
        const auto& versions = all[i];
        const auto exe = dir->path() / ("omp" + std::to_string(i));
        const auto seen = dir->path() / ("args" + std::to_string(i));
        const auto ready = R"({"type":"ready","protocolVersion":1,"supportedProtocolVersions":)" + versions + "}";
        const auto body = "#!/bin/sh\necho \"$@\" > '" + seen.string() + "'\necho '" + ready + "'\nread ignored\n";
        REQUIRE(loa::runtime::exec::write_executable(exe, body));
        auto offered = probe.protocol_versions(exe);
        REQUIRE_MESSAGE(offered, offered.error());
        json::Array arr;
        for (auto v : *offered) arr.emplace_back(std::uint64_t{v});
        CHECK(json::dump(Json(arr)) == versions);
        auto args = read_file(seen);
        while (!args.empty() && (args.back() == '\n' || args.back() == ' ')) args.pop_back();
        CHECK(args == "--mode rpc --no-session");
    }
}

TEST_CASE("update_gates: diff allows added fields and reports changes") {
    const std::vector<Json> expected{parse(R"({"id":"a","type":"message","message":{"content":[{"text":"hi"}]}})")};
    const std::vector<Json> added{parse(R"({"id":"a","type":"message","runtime":{},"message":{"content":[{"text":"hi","x":1}]}})")};
    CHECK(detail::diff_entries(expected, added).empty());
    const std::vector<Json> changed{parse(R"({"id":"a","type":"message","message":{"content":[{"text":"bye"}]}})"),
                                    parse(R"({"id":"b","type":"custom"})")};
    CHECK(detail::diff_entries(expected, changed) ==
          std::vector<std::string>{"~ a.message.content[0].text: \"hi\" -> \"bye\"", "+ entry b (custom) unexpected"});
    CHECK(detail::diff_entries(expected, {}) == std::vector<std::string>{"- entry a (message) missing"});
}

TEST_CASE("update_gates: goldens parse, branch and take the cwd") {
    const auto goldens = detail::goldens();
    REQUIRE(goldens.size() == 2);
    for (const auto& golden : goldens) {
        INFO(golden.name);
        auto entries = detail::entries(golden.jsonl);
        REQUIRE(entries);
        REQUIRE_FALSE(entries->empty());
        auto path = detail::branch(*entries, detail::entry_id(entries->back()));
        REQUIRE_FALSE(path.empty());
        CHECK(*path.back()->find("id") == *entries->back().find("id"));
        REQUIRE(path.front()->find("parentId"));
        CHECK(path.front()->find("parentId")->is_null());
    }
    auto branched = detail::entries(goldens[1].jsonl);
    REQUIRE(branched);
    std::vector<std::string_view> ids;
    for (const auto* e : detail::branch(*branched, "c0000010"))
        if (auto id = detail::entry_id(*e)) ids.push_back(*id);
    CHECK(std::ranges::find(ids, "c0000003") == ids.end());
    auto text = detail::with_cwd(goldens[0].jsonl, "/tmp/x");
    REQUIRE(text);
    auto header = parse(text->substr(0, text->find('\n')));
    CHECK(*header.find("cwd") == Json("/tmp/x"));
}

TEST_CASE("update_gates: embedded goldens match the files and round trip byte-identically") {
    for (const auto& golden : detail::goldens()) {
        INFO(golden.name);
        const auto file = read_file(std::filesystem::path(LOA_GOLDEN_DIR) / "updater" / "omp_sessions" / (std::string(golden.name) + ".jsonl"));
        CHECK(file == golden.jsonl);
        std::istringstream in(file);
        std::size_t count = 0;
        for (std::string line; std::getline(in, line); ++count) {
            CHECK(json::dump(parse(line)) == line);
        }
        CHECK(count > 0);
    }
}

TEST_CASE("update_gates: smol reviewer parses the reply and rejects junk") {
    adapters::SmolReviewer review(std::make_unique<Canned>(R"({"breaking": ["edit: path renamed"], "removed": []})"));
    auto reviewed = review.review("## 2.0.0\n\n- fix");
    REQUIRE(reviewed);
    CHECK(reviewed->breaking == std::vector<std::string>{"edit: path renamed"});
    adapters::SmolReviewer junk(std::make_unique<Canned>("Sure! Nothing breaking here."));
    CHECK_FALSE(junk.review("- fix"));
}
