#include <doctest/doctest.h>

#include <algorithm>
#include <cstdlib>
#include <fstream>
#include <tuple>

#include "../kit/support.hpp"
#include "loa/harness/native.hpp"
#include "loa/runtime/tempfile.hpp"

namespace native = loa::harness::native;
namespace fs = std::filesystem;

namespace {

void write(const fs::path& path, std::string_view text) {
    fs::create_directories(path.parent_path());
    std::ofstream(path, std::ios::binary) << text;
}

void set_mtime_ms(const fs::path& path, std::int64_t ms) {
    const auto sys = std::chrono::system_clock::time_point(std::chrono::milliseconds(ms));
    fs::last_write_time(path, std::chrono::clock_cast<fs::file_time_type::clock>(sys));
}

// `CODEX_HOME` would move codex discovery away from the test home.
struct NoCodexHome {
    NoCodexHome() {
        if (const char* v = std::getenv("CODEX_HOME")) saved = v;
        ::unsetenv("CODEX_HOME");
    }
    ~NoCodexHome() {
        if (saved) ::setenv("CODEX_HOME", saved->c_str(), 1);
    }
    std::optional<std::string> saved;
};

} // namespace

TEST_CASE("native: sessions are found and recognized") {
    NoCodexHome guard;
    auto home = loa::runtime::TempDir::create().value();
    const fs::path h = home.path();
    struct Case { std::string_view path, text; std::optional<std::string_view> harness; };
    const Case cases[] = {
        {".omp/agent/sessions/-Projects-a/2026_x.jsonl", "{\"type\":\"title\",\"title\":\"Sorter\"}\n", native::OMP},
        {".claude/projects/-home-a/u.jsonl", "{\"type\":\"user\",\"sessionId\":\"u\"}\n", native::CLAUDE},
        {".codex/sessions/2026/11/19/rollout-1.jsonl", "{\"type\":\"session_meta\"}\n", native::CODEX},
        {".codex/sessions/2026/11/19/other.txt", "x", std::nullopt},
        {"elsewhere/export.json", "{\"info\":{\"id\":\"ses_1\"},\"messages\":[]}", native::OPENCODE},
        {"elsewhere/omp.jsonl", "{\"type\":\"session\",\"version\":3}\n", native::OMP},
        {"elsewhere/rollout-9.jsonl", "{}\n", native::CODEX},
        {"elsewhere/notes.jsonl", "{\"a\":1}\n", std::nullopt},
    };
    for (const auto& c : cases) write(h / c.path, c.text);
    for (const auto& c : cases) {
        INFO(c.path);
        CHECK(native::harness_of(h / c.path) == c.harness);
    }
    CHECK_FALSE(native::harness_of(h / "elsewhere/missing.jsonl"));

    using Kind = std::tuple<std::string, std::optional<std::string>, std::optional<std::string>>;
    std::vector<Kind> kinds;
    for (const auto& s : native::discover(h)) kinds.emplace_back(s.harness, s.project, s.title);
    std::ranges::sort(kinds);
    CHECK(kinds == std::vector<Kind>{{"claude-code", "-home-a", std::nullopt},
                                     {"codex", std::nullopt, std::nullopt},
                                     {"omp", "-Projects-a", "Sorter"}});
}

TEST_CASE("native: discovery order and metadata match the Rust golden") {
    NoCodexHome guard;
    auto home = loa::runtime::TempDir::create().value();
    const fs::path h = home.path();
    struct File { std::string_view path, body; std::int64_t ms; };
    const File files[] = {
        {".omp/agent/sessions/-Projects-a/2026_x.jsonl", "{\"type\":\"title\",\"title\":\"Sorter\"}\n", 1'700'000'003'000},
        {".omp/agent/sessions/-Projects-a/2026_y.jsonl", "{\"type\":\"session\"}\n", 1'700'000'001'000},
        {".omp/agent/sessions/-Projects-b/b.jsonl", "", 1'700'000'002'000},
        {".omp/agent/sessions/top.jsonl", "{}\n", 1'700'000'009'000},
        {".claude/projects/-home-a/u.jsonl", "{\"type\":\"user\",\"sessionId\":\"u\"}\n", 1'700'000'002'000},
        {".claude/projects/-home-a/v.json", "{}", 1'700'000'005'000},
        {".codex/sessions/2026/11/19/rollout-1.jsonl", "{\"type\":\"session_meta\"}\n", 1'700'000'004'500},
        {".codex/sessions/2026/11/rollout-2.jsonl", "{}\n", 1'700'000'008'000},
        {".codex/sessions/2026/11/19/other.txt", "x", 1'700'000'004'000},
    };
    for (const auto& f : files) {
        write(h / f.path, f.body);
        set_mtime_ms(h / f.path, f.ms);
    }
    std::string list;
    for (const auto& s : native::discover(h)) {
        list += s.harness + "\t" + fs::relative(s.path, h).string() + "\t" + s.project.value_or("") + "\t" +
                s.title.value_or("") + "\t" + std::to_string(s.modified) + "\t" + std::to_string(s.size) + "\n";
    }
    CHECK(list == loa::harness::test::read_file(loa::harness::test::golden("harness/discover.tsv")));
}

TEST_CASE("native: CODEX_HOME moves codex discovery") {
    NoCodexHome guard;
    auto home = loa::runtime::TempDir::create().value();
    auto codex = loa::runtime::TempDir::create().value();
    write(codex.path() / "sessions/2026/01/02/rollout-a.jsonl", "{}\n");
    ::setenv("CODEX_HOME", codex.path().c_str(), 1);
    auto found = native::discover_codex(home.path());
    REQUIRE(found.size() == 1);
    CHECK(found[0].harness == native::CODEX);
    ::setenv("CODEX_HOME", "", 1);
    CHECK(native::discover_codex(home.path()).empty());
}
