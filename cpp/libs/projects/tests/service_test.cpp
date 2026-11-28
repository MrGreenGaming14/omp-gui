// Port of the `LiveSessions` and `ProjectService` tests of `crates/loa-projects/src/tests.rs`.

#include <doctest/doctest.h>

#include <asio/executor_work_guard.hpp>
#include <filesystem>
#include <set>
#include <thread>

#include "loa/bus/journal.hpp"
#include "loa/projects/links.hpp"
#include "loa/projects/service.hpp"
#include "loa/projects/sessions.hpp"
#include "loa/runtime/tempfile.hpp"
#include "testing.hpp"

using namespace loa::projects;
using namespace loa::projects::test;
using loa::settings::ProjectSettings;
namespace fs = std::filesystem;
namespace bus = loa::bus;

TEST_CASE("live_sessions_read_and_write_live_and_stored_sessions") {
    auto dir = loa::runtime::TempDir::create().value();
    auto store = bus::Store::make(dir.path()).value();
    auto b = std::make_shared<bus::Bus>();
    live(*b, store, "closed", "loa", "/home/u/loa", 1);
    b->close("closed");
    auto open = live(*b, store, "open", "loa", "/home/u/loa", 1);
    // A session without a PROJECT attr belongs to no project, whatever its cwd.
    auto stray = live(*b, store, "stray", std::nullopt, "/home/u/loa", 1);
    auto web = live(*b, store, "web", std::nullopt, "/home/u/web", 1);
    const LiveSessions sessions(b, store);
    std::vector<std::string> listed;
    const auto infos = sessions.list("loa").value();
    for (const auto& i : infos) listed.push_back(i.id);
    std::ranges::sort(listed);
    CHECK(listed == std::vector<std::string>{"closed", "open"});
    CHECK(sessions.list("web").value().empty());
    CHECK(sessions.load("../etc").value() == nullptr);
    auto dom = sessions.load("closed").value();
    REQUIRE(sessions.commit("closed", *exclude_draft(*dom, true)).has_value());
    CHECK(is_excluded(store.open("closed").value().first.load().value()));
    dom = sessions.load("open").value();
    REQUIRE(sessions.commit("open", *exclude_draft(*dom, true)).has_value());
    CHECK(open->with_dom([](const loa::core::Dom& d) { return is_excluded(d); }));
}

namespace {

// A project service over live sessions and file settings in a temp dir.
struct Env {
    loa::runtime::TempDir dir = loa::runtime::TempDir::create().value();
    std::shared_ptr<bus::Bus> b = std::make_shared<bus::Bus>();
    bus::Store store = bus::Store::make(dir.path() / "sessions").value();
    asio::io_context context;
    std::optional<ProjectService> service;
    std::vector<std::shared_ptr<bus::SessionWriter>> writers;

    Env() {
        auto settings = std::make_shared<FileSettingsStore>(dir.path() / "projects");
        auto [s, _] = ProjectService::start(LiveSessions(b, store), settings, std::make_shared<LocalOnly>(),
                                            std::make_shared<Fake>(), ServiceConfig{}, context);
        service.emplace(std::move(s));
    }

    ~Env() {
        // Dropping the service closes the driver; run it to completion.
        service.reset();
        context.run();
    }

    void open(std::string_view id, std::string_view project, std::string_view cwd, std::int64_t at) {
        writers.push_back(live(*b, store, id, project, cwd, at));
    }
};

ProjectSettings in_folder(const fs::path& folder) {
    ProjectSettings s;
    s.folder = folder;
    return s;
}

using Row = std::tuple<std::string, std::size_t, std::optional<std::string>>;

std::vector<Row> rows(const std::vector<ProjectSummary>& projects) {
    std::vector<Row> out;
    for (const auto& p : projects) out.emplace_back(p.name, p.sessions, p.cwd);
    return out;
}

} // namespace

TEST_CASE("unreadable_ideas_still_show_the_rest_of_the_project") {
    Env env;
    env.open("a", "loa", "/home/u/loa", 10);
    fs::create_directories(env.dir.path() / "projects");
    write_file(env.dir.path() / "projects" / "loa.ideas.md", std::string("\xff\xfeh\0i\0", 6));
    auto overview = env.service->overview("loa").value();
    REQUIRE_FALSE(overview.ideas.has_value());
    CHECK(overview.ideas.error().find("loa.ideas.md") != std::string::npos);
    CHECK(overview.sessions.size() == 1);
}

TEST_CASE("sessions_without_a_project_belong_to_no_project") {
    Env env;
    env.open("a", "loa", "/home/u/loa", 10);
    env.open("b", "loa", "/home/u/scratch", 20);
    REQUIRE(env.service->move_session("b", std::nullopt).has_value());
    CHECK(rows(env.service->projects().value()) == std::vector<Row>{{"loa", 1, "/home/u/loa"}});
}

TEST_CASE("created_projects_list_with_their_folder") {
    Env env;
    env.open("a", "loa", "/home/u/loa", 10);
    REQUIRE(env.service->create("  docs  ", in_folder("/home/u/docs")).has_value());
    REQUIRE(env.service->create("empty", ProjectSettings{}).has_value());
    // A project only sessions named until now becomes a stored one.
    REQUIRE(env.service->create("loa", in_folder("/srv/loa")).has_value());
    CHECK(rows(env.service->projects().value()) ==
          std::vector<Row>{{"loa", 1, "/srv/loa"}, {"docs", 0, "/home/u/docs"}, {"empty", 0, std::nullopt}});
}

TEST_CASE("create_rejects_a_blank_or_taken_name") {
    Env env;
    CHECK(env.service->create("  ", ProjectSettings{}).error().kind == ErrorKind::EmptyName);
    CHECK(env.service->projects().value().empty());
    REQUIRE(env.service->create("docs", in_folder("/home/u/docs")).has_value());
    auto again = env.service->create(" docs ", ProjectSettings{});
    REQUIRE_FALSE(again.has_value());
    CHECK(again.error() == Error::project_exists("docs"));
    CHECK(env.service->overview("docs").value().cwd == std::optional<std::string>("/home/u/docs"));
}

TEST_CASE("move_session_sets_and_clears_the_project") {
    Env env;
    env.open("a", "loa", "/home/u/loa", 10);
    REQUIRE(env.service->move_session("a", "web").has_value());
    auto moved = env.service->projects().value();
    REQUIRE(moved.size() == 1);
    CHECK(moved[0].name == "web");
    CHECK(moved[0].sessions == 1);
    REQUIRE(env.service->move_session("a", std::nullopt).has_value());
    CHECK(env.service->projects().value().empty());
    CHECK(env.service->move_session("nope", "web").error().kind == ErrorKind::SessionNotFound);
}

TEST_CASE("a_project_in_the_temp_folder_gets_its_own_folder_inside_it") {
    Env env;
    const auto temp = fs::temp_directory_path();
    auto folder = env.service->create("scratch", in_folder(temp)).value().value();
    CHECK(folder.parent_path() == temp);
    CHECK(fs::is_directory(folder));
    CHECK(env.service->overview("scratch").value().settings.folder == folder);

    // Choosing the temp folder again isolates again; any other folder is kept as is.
    auto again = env.service->set_folder("scratch", temp).value();
    CHECK(again != folder);
    CHECK(again.parent_path() == temp);
    auto elsewhere = loa::runtime::TempDir::create().value();
    CHECK(env.service->set_folder("scratch", elsewhere.path()).value() == elsewhere.path());
    fs::remove(folder);
    fs::remove(again);
}

TEST_CASE("the_project_folder_links_to_its_sessions_directories_and_moves") {
    Env env;
    auto root = loa::runtime::TempDir::create().value();
    const auto dir = [&](std::string_view name) {
        auto d = root.path() / name;
        fs::create_directories(d);
        return d;
    };
    const auto home = dir("home"), api = dir("api"), web = dir("web"), moved = dir("moved");
    const auto linked = [](const fs::path& folder) {
        std::set<fs::path> out;
        std::error_code ec;
        for (fs::directory_iterator it(folder / links::LINKS_DIR, ec), end; !ec && it != end; it.increment(ec))
            if (fs::is_symlink(it->symlink_status())) out.insert(fs::read_symlink(it->path()));
        return out;
    };
    REQUIRE(env.service->create("launch", in_folder(home)).has_value());
    env.open("a", "launch", api.string(), 10);
    env.open("b", "launch", web.string(), 20);
    env.open("c", "launch", home.string(), 30);

    REQUIRE(env.service->projects().has_value());
    CHECK(linked(home) == std::set<fs::path>{api, web});

    REQUIRE(env.service->move_session("b", std::nullopt).has_value());
    REQUIRE(env.service->projects().has_value());
    CHECK(linked(home) == std::set<fs::path>{api});

    REQUIRE(env.service->set_folder("launch", moved).has_value());
    CHECK_FALSE(fs::exists(home / links::LINKS_DIR));
    REQUIRE(env.service->projects().has_value());
    CHECK(linked(moved) == std::set<fs::path>{api, home});

    // Deleting keeps the sessions, in no project, and takes the links.
    CHECK(env.service->remove("launch").value() == 2);
    CHECK_FALSE(fs::exists(moved / links::LINKS_DIR));
    CHECK(env.service->projects().value().empty());
    CHECK(env.service->overview("launch").value().sessions.empty());
    const LiveSessions sessions(env.b, env.store);
    CHECK(sessions.load("a").value() != nullptr);
}

TEST_CASE("context_and_exclude_through_the_service") {
    Env env;
    env.open("a", "loa", "/home/u/loa", 10);
    CHECK_FALSE(env.service->context("loa", std::nullopt, "").value().has_value());
    ProjectSettings settings;
    settings.instructions = "Use rustls.";
    REQUIRE(env.service->save_settings("loa", settings, std::nullopt).has_value());
    auto md = env.service->context("loa", std::nullopt, "").value();
    REQUIRE(md.has_value());
    CHECK(md->starts_with("# Project context: loa\n\n## Shared instructions\n\nUse rustls.\n"));
    CHECK(env.service->set_excluded("a", true).value());
    CHECK_FALSE(env.service->set_excluded("a", true).value());
    CHECK(env.service->overview("loa").value().sessions.at(0).excluded);
    auto [specs, tools] = env.service->host_tools();
    REQUIRE(specs.size() == 2);
    CHECK(specs[0].name == "project_read");
    CHECK(specs[1].name == "project_search");
}
