#include <doctest/doctest.h>

#include <chrono>
#include <filesystem>
#include <fstream>
#include <string>

#include "loa/harness/omp/native.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/tempfile.hpp"

namespace fs = std::filesystem;
namespace json = loa::runtime::json;
namespace omp_native = loa::harness::omp::native;

namespace {

void write(const fs::path& path, std::string_view text, std::int64_t mtime_ms = -1) {
    fs::create_directories(path.parent_path());
    std::ofstream(path, std::ios::binary) << text;
    if (mtime_ms >= 0) {
        const auto sys = std::chrono::system_clock::time_point(std::chrono::milliseconds(mtime_ms));
        fs::last_write_time(path, std::chrono::clock_cast<fs::file_time_type::clock>(sys));
    }
}

std::string header(std::string_view id, const fs::path& cwd, std::string_view title = {}) {
    json::Object fields{{"type", "session"}, {"version", 3}, {"id", id}, {"cwd", cwd.string()}};
    if (!title.empty()) fields.emplace_back("title", title);
    return json::dump(fields) + "\n";
}

} // namespace

TEST_CASE("omp native history title precedence and bounded prefix") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp.has_value());
    const auto project = tmp->path() / "project";
    fs::create_directories(project);
    const auto file = tmp->path() / "agent/sessions/p/a.jsonl";
    write(file, "{\"type\":\"title\",\"title\":\"Prefix\"}\n" + header("a", project, "Header"));
    auto session = omp_native::project_session(file, project);
    REQUIRE(session.has_value());
    REQUIRE(session->has_value());
    CHECK((*session)->title == "Prefix");
    const auto fallback = tmp->path() / "agent/sessions/p/stem.jsonl";
    write(fallback, "{\"type\":\"title\",\"title\":\"  \"}\n" + header("b", project));
    session = omp_native::project_session(fallback, project);
    REQUIRE(session.has_value());
    REQUIRE(session->has_value());
    CHECK((*session)->title == "stem");
    write(file, header("a", project, "Bounded") + std::string(256 * 1024, 'x') + "\xff\xfe");
    session = omp_native::project_session(file, project);
    REQUIRE(session.has_value());
    REQUIRE(session->has_value());
    CHECK((*session)->title == "Bounded");
}

TEST_CASE("omp native history rejects malformed unsupported and mismatched headers") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp.has_value());
    const auto project = tmp->path() / "project";
    const auto other = tmp->path() / "other";
    fs::create_directories(project);
    fs::create_directories(other);
    const auto file = tmp->path() / "session.jsonl";
    for (const std::string& text : {std::string(), std::string("not json\n"),
                                    std::string("[1,2,3]\n"),
                                    std::string("\xff\xfe\n"),
                                    std::string("{\"type\":\"title\",\"title\":\"Only\"}\n"),
                                    std::string("{\"type\":\"title\",\"title\":\"T\"}\n{\"type\":\"message\"}\n"),
                                    std::string("{\"type\":\"title\"}\n{\"type\":\"title\"}\n") + header("id", project),
                                    std::string("{\"type\":\"message\"}\n") + header("id", project),
                                    std::string("{\"type\":\"session\",\"id\":\"s\"}\n"),
                                    std::string("{\"type\":\"session\",\"id\":\"s\",\"cwd\":\"relative\"}\n"),
                                    header("", project), header("  ", project)}) {
        write(file, text);
        auto session = omp_native::project_session(file, project);
        REQUIRE(session.has_value());
        CHECK_FALSE(session->has_value());
    }
    write(file, header("outside", other));
    auto session = omp_native::project_session(file, project);
    REQUIRE(session.has_value());
    CHECK_FALSE(session->has_value());
    write(file, header("alias", other / "../project/."));
    session = omp_native::project_session(file, project);
    REQUIRE(session.has_value());
    REQUIRE(session->has_value());
    CHECK((*session)->id == "alias");
    const auto link = tmp->path() / "link";
    fs::create_directory_symlink(project, link);
    write(file, header("symlink", link));
    session = omp_native::project_session(file, project);
    REQUIRE(session.has_value());
    REQUIRE(session->has_value());
    CHECK((*session)->id == "symlink");
}

TEST_CASE("omp native history matches exact folder depth orders and deduplicates") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp.has_value());
    const auto agent = tmp->path() / "agent";
    const auto project = tmp->path() / "project";
    const auto other = tmp->path() / "other";
    fs::create_directories(project);
    fs::create_directories(other);
    write(agent / "sessions/p/old.jsonl", header("dup", project, "old"), 1700000010000);
    write(agent / "sessions/q/new.jsonl", header("dup", project, "new"), 1700000030000);
    write(agent / "sessions/p/tie-a.jsonl", header("tie-a", project), 1700000020000);
    write(agent / "sessions/p/tie-b.jsonl", header("tie-b", project), 1700000020000);
    write(agent / "sessions/p/foreign.jsonl", header("other", other));
    write(agent / "sessions/stray.jsonl", header("stray", project));
    write(agent / "sessions/p/deep/no.jsonl", header("deep", project));
    auto history = omp_native::project_history(agent, project);
    REQUIRE(history.has_value());
    REQUIRE(history->size() == 3);
    CHECK((*history)[0].id == "dup");
    CHECK((*history)[0].title == "new");
    CHECK((*history)[1].id == "tie-a");
    CHECK((*history)[2].id == "tie-b");
    CHECK((*history)[0].modified == 1700000030000);
    const auto copy = tmp->path() / "copied.jsonl";
    fs::copy_file(agent / "sessions/q/new.jsonl", copy);
    auto copied = omp_native::project_session(copy, project);
    REQUIRE(copied.has_value());
    REQUIRE(copied->has_value());
    CHECK((*copied)->path == copy);
}

TEST_CASE("omp native history deletion removes copies and sidecars but not other sessions") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp.has_value());
    const auto agent = tmp->path() / "agent";
    const auto project = tmp->path() / "project";
    const auto other = tmp->path() / "other";
    fs::create_directories(project);
    fs::create_directories(other);
    const auto doomed = agent / "sessions/p/doomed.jsonl";
    const auto copy = agent / "sessions/q/copy.jsonl";
    const auto kept = agent / "sessions/p/kept.jsonl";
    const auto foreign = agent / "sessions/o/foreign.jsonl";
    write(doomed, header("doomed", project));
    write(copy, header("doomed", project));
    write(kept, header("kept", project));
    write(foreign, header("foreign", other));
    auto sidecar = doomed;
    sidecar.replace_extension();
    fs::create_directories(sidecar / "subagents");
    write(sidecar / "subagents/task.jsonl", "{}\n");
    auto not_sidecar = copy;
    not_sidecar.replace_extension();
    write(not_sidecar, "file\n");
    REQUIRE(omp_native::delete_project_session(agent, project, "doomed").has_value());
    CHECK_FALSE(fs::exists(doomed));
    CHECK_FALSE(fs::exists(copy));
    CHECK_FALSE(fs::exists(sidecar));
    CHECK(fs::is_regular_file(not_sidecar));
    CHECK(fs::is_regular_file(kept));
    CHECK(fs::is_regular_file(foreign));
    for (std::string_view id : {"doomed", "foreign"}) {
        auto removed = omp_native::delete_project_session(agent, project, id);
        REQUIRE_FALSE(removed.has_value());
        CHECK(removed.error().code() == loa::ErrorCode::not_found);
    }
}

TEST_CASE("omp native history leaves symlink sidecars alone") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp.has_value());
    const auto project = tmp->path() / "project";
    const auto agent = tmp->path() / "agent";
    const auto target = tmp->path() / "target";
    fs::create_directories(project);
    fs::create_directories(target);
    const auto session = agent / "sessions/p/symlink.jsonl";
    write(session, header("symlink", project));
    auto sidecar = session;
    sidecar.replace_extension();
    fs::create_directory_symlink(target, sidecar);
    REQUIRE(omp_native::delete_project_session(agent, project, "symlink").has_value());
    CHECK_FALSE(fs::exists(session));
    CHECK(fs::is_symlink(sidecar));
    CHECK(fs::is_directory(target));
}

TEST_CASE("omp native history keeps a session when its sidecar removal fails") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp.has_value());
    const auto agent = tmp->path() / "agent";
    const auto project = tmp->path() / "project";
    fs::create_directories(project);
    const auto session = agent / "sessions/p/stuck.jsonl";
    write(session, header("stuck", project));
    auto sidecar = session;
    sidecar.replace_extension();
    fs::create_directory(sidecar);
    auto matching = omp_native::detail::matching_sessions(agent, project);
    REQUIRE(matching.has_value());
    auto removed = omp_native::detail::remove_sessions(*matching, [](const fs::path&) -> std::expected<void, loa::Error> {
        return std::unexpected(loa::Error(loa::ErrorCode::permission_denied, "held open"));
    });
    REQUIRE_FALSE(removed.has_value());
    CHECK(removed.error().code() == loa::ErrorCode::permission_denied);
    CHECK(removed.error().to_string().find(sidecar.string()) != std::string::npos);
    CHECK(fs::is_regular_file(session));
    REQUIRE(omp_native::delete_project_session(agent, project, "stuck").has_value());
    CHECK_FALSE(fs::exists(session));
    CHECK_FALSE(fs::exists(sidecar));
}

TEST_CASE("omp native history missing dir is empty and real IO errors propagate") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp.has_value());
    const auto project = tmp->path() / "project";
    fs::create_directories(project);
    auto missing = omp_native::project_history(tmp->path() / "missing-agent", project);
    REQUIRE(missing.has_value());
    CHECK(missing->empty());
    auto file = omp_native::project_session(tmp->path() / "missing.jsonl", project);
    REQUIRE_FALSE(file.has_value());
    CHECK(file.error().code() == loa::ErrorCode::not_found);
    const auto broken = tmp->path() / "broken-agent";
    fs::create_directories(broken);
    write(broken / "sessions", "not a directory");
    CHECK_FALSE(omp_native::project_history(broken, project).has_value());
}
