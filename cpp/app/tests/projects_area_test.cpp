// The projects area driven through `App::update` (port of `loa-app/src/areas/projects/tests.rs`).

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <string>
#include <vector>

#include <doctest/doctest.h>

#include "areas/projects/projects.hpp"
#include "loa/app/app.hpp"
#include "loa/app/routes.hpp"
#include "loa/core/schema.hpp"
#include "loa/harness/omp/native.hpp"
#include "loa/projects/links.hpp"
#include "loa/runtime/json.hpp"
#include "loa/ui/shell/sidebar.hpp"
#include "support/test_app.hpp"

using namespace loa::app;
using loa::app::testing::TestApp;
namespace fs = std::filesystem;
namespace pa = loa::app::areas::projects;
namespace screen = loa::ui::settings::projects;
namespace create = loa::ui::settings::projects::create;
namespace vs = loa::view::settings::projects;
namespace nav = loa::view::shell::sidebar_intent;
namespace sidebar = loa::ui::shell::sidebar;
namespace layout = loa::ui::shell::layout;

namespace {

using State = pa::State;

template <class M>
loa::app::Message area(M message) {
    return State::message(pa::Message(std::move(message)));
}

loa::app::Message local(screen::Local l) { return area(pa::msg::Screen{screen::Message(std::move(l))}); }
loa::app::Message screen_intent(screen::Intent i) { return area(pa::msg::Screen{screen::Message(std::move(i))}); }
loa::app::Message form(create::Local l) { return area(pa::msg::Form{create::Message(std::move(l))}); }

loa::app::Message sidebar_message(sidebar::Message m) { return msg::Shell{layout::msg::Sidebar{std::move(m)}}; }
loa::app::Message sidebar_intent(loa::view::shell::SidebarIntent i) { return sidebar_message(sidebar::msg::Emit{std::move(i)}); }

const State& projects(const TestApp& test) { return test.area<State>(); }

// Makes `project` the active one, then starts a session with `text` in it.
std::string chat_in(TestApp& test, const std::string& project, std::string_view text) {
    test.send(area(to::projects::Activate{project}));
    return test.session(text);
}

// The project recorded on session `id`'s root, as its writer holds it.
std::optional<std::string> session_project(const App& app, std::string_view id) {
    const auto* session = app.ctx.runtime.sessions().get(id);
    REQUIRE(session != nullptr);
    return session->writer()->with_dom([](const loa::core::Dom& dom) -> std::optional<std::string> {
        const auto* root = dom.root();
        if (!root) return std::nullopt;
        auto value = root->str_attr(loa::core::attr::PROJECT);
        return value ? std::optional<std::string>(*value) : std::nullopt;
    });
}

// The directories `<folder>/.omgui` links to.
std::vector<fs::path> linked(const fs::path& folder) {
    std::vector<fs::path> targets;
    std::error_code ec;
    for (const auto& entry : fs::directory_iterator(folder / loa::projects::links::LINKS_DIR, ec)) {
        auto target = fs::read_symlink(entry.path(), ec);
        if (!ec) targets.push_back(target);
    }
    std::ranges::sort(targets);
    return targets;
}

void write_file(const fs::path& path, std::string_view bytes) {
    fs::create_directories(path.parent_path());
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    out.write(bytes.data(), static_cast<std::streamsize>(bytes.size()));
}

std::string read_file(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(in), {}};
}

fs::path saved_omp_session(const fs::path& dir, const std::string& id, const fs::path& cwd, const std::string& title) {
    auto path = dir / (id + ".jsonl");
    auto quote = [](const std::string& s) { return loa::runtime::json::dump(loa::runtime::json::Value(s)); };
    auto header = R"({"type":"session","version":3,"id":)" + quote(id) + R"(,"cwd":)" + quote(cwd.string()) +
                  R"(,"title":)" + quote(title) + "}";
    std::string message =
        R"({"type":"message","id":"first","parentId":null,"message":{"role":"user","content":"Read the project guide."}})";
    write_file(path, header + "\n" + message + "\n");
    return path;
}

void create_project(TestApp& test, const std::string& name, const fs::path& folder) {
    test.send(area(to::projects::New{}));
    test.send(form(create::local::Name{name}));
    test.send(form(create::local::Folder{folder.string()}));
    test.send(form(create::local::Create{}));
}

// The ids of the shown project's terminal history, sorted.
std::vector<std::string> listed_history(const TestApp& test) {
    const auto* history = projects(test).screen()->omp_history();
    REQUIRE(history != nullptr);
    const auto* ready = std::get_if<vs::omp_history::Ready>(history);
    REQUIRE_MESSAGE(ready != nullptr, "expected a loaded terminal history");
    std::vector<std::string> ids;
    for (const auto& row : ready->rows) ids.push_back(row.id);
    std::ranges::sort(ids);
    return ids;
}

std::size_t stored(const TestApp& test) {
    auto list = test.app->ctx.runtime.library()->store().list();
    REQUIRE(list.has_value());
    return list->size();
}

std::string last_context(const TestApp& test) {
    auto configs = test.harness.seen().configs;
    if (configs.empty() || !configs.back().context) return {};
    return *configs.back().context;
}

fs::path notes_path(const TestApp& test, std::string_view project) {
    return test.app->ctx.services.storage.dirs.data / "projects" / (std::string(project) + ".ideas.md");
}

bool contains(std::string_view hay, std::string_view needle) { return hay.find(needle) != std::string_view::npos; }

} // namespace

TEST_CASE("projects: lists terminal history then opens one independent copy") {
    TestApp test;
    auto folder = test.dir.path() / "Existing project";
    auto subfolder = folder / "child";
    fs::create_directories(subfolder);
    auto storage = test.dir.path() / "omp/sessions/encoded-workspace";
    auto source = saved_omp_session(storage, "terminal-one", folder, "Saved terminal chat");
    saved_omp_session(storage, "terminal-two", subfolder, "Child folder chat");
    auto original = read_file(source);
    create_project(test, "Existing", folder);
    CHECK(listed_history(test) == std::vector<std::string>{"terminal-one"});
    CHECK_MESSAGE(stored(test) == 0, "listing must not import");
    test.send(screen_intent(vs::intent::OpenOmpHistory{"terminal-one"}));
    auto id = test.active();
    REQUIRE_MESSAGE(id.has_value(), "the imported conversation is open");
    CHECK(session_project(*test.app, *id) == "Existing");
    auto dom = test.app->ctx.runtime.library()->load(*id);
    REQUIRE(dom.has_value());
    bool found = false;
    for (const auto& node : dom->iter()) found = found || node.text == "Read the project guide.";
    CHECK(found);
    auto file = dom->root()->str_attr(loa::harness::omp::native::X_SESSION_FILE);
    REQUIRE(file.has_value());
    fs::path copy(*file);
    CHECK(copy != source);
    CHECK(read_file(copy) == original);
    CHECK(read_file(source) == original);

    test.reopen();
    test.send(area(to::projects::Open{"Existing"}));
    CHECK(listed_history(test).empty());
    REQUIRE(projects(test).screen()->overview() != nullptr);
    CHECK(projects(test).screen()->overview()->sessions.at(0).id == *id);
    CHECK(stored(test) == 1);
}

TEST_CASE("projects: a changed terminal file cannot be imported into the wrong project") {
    TestApp test;
    auto folder = test.dir.path() / "project";
    auto other = test.dir.path() / "other";
    fs::create_directories(folder);
    fs::create_directories(other);
    auto storage = test.dir.path() / "omp/sessions/saved";
    saved_omp_session(storage, "chat", folder, "Moving chat");
    create_project(test, "Project", folder);
    saved_omp_session(storage, "chat", other, "Moving chat");
    test.send(screen_intent(vs::intent::OpenOmpHistory{"chat"}));
    CHECK(contains(test.notice(), "terminal session changed"));
    CHECK(stored(test) == 0);
    CHECK(test.app->ctx.screen() == "projects");
}

TEST_CASE("projects: an obsolete overview cannot restore the previous folder's history") {
    TestApp test;
    auto old_dir = test.dir.path() / "old";
    auto new_dir = test.dir.path() / "new";
    fs::create_directories(old_dir);
    fs::create_directories(new_dir);
    auto storage = test.dir.path() / "omp/sessions/saved";
    saved_omp_session(storage, "old-chat", old_dir, "Old chat");
    saved_omp_session(storage, "new-chat", new_dir, "New chat");
    create_project(test, "Project", old_dir);
    auto obsolete = projects(test).overview_request();
    test.send(area(pa::msg::FolderChosen{pa::pick::Project{"Project"}, std::optional<fs::path>(new_dir)}));
    loa::settings::ProjectOverview stale{"Project", old_dir.string(), loa::settings::ProjectSettings{.folder = old_dir},
                                         std::unexpected(std::string("not read")), {}};
    test.send(area(pa::msg::Overview{obsolete, "Project", std::move(stale)}));
    CHECK(listed_history(test) == std::vector<std::string>{"new-chat"});
}

TEST_CASE("projects: an unfinished import is not visible in the session store") {
    TestApp test;
    const auto& library = test.app->ctx.runtime.library();
    auto cwd = test.dir.path();
    auto native = saved_omp_session(cwd / "external", "source", cwd, "Saved chat");
    auto result = library->import_staged("attempt", "owner", "Project",
                                         [&](const fs::path&, const fs::path&, loa::core::PatchSink& sink) -> loa::session::Result<> {
                                             REQUIRE(loa::harness::omp::native::import(native, sink, "owner").has_value());
                                             CHECK(stored(test) == 0);
                                             return std::unexpected(loa::session::Error::port("import interrupted"));
                                         });
    REQUIRE_FALSE(result.has_value());
    CHECK(result.error().kind() == loa::session::Error::Kind::Port);
    CHECK(result.error().detail() == "import interrupted");
    CHECK(stored(test) == 0);
}

TEST_CASE("projects: open and pin from the palette") {
    TestApp test;
    chat_in(test, "alpha", "hello");
    auto titles = test.palette_titles();
    auto at = [&](std::string_view title) {
        auto it = std::ranges::find(titles, title);
        REQUIRE_MESSAGE(it != titles.end(), title);
        return it - titles.begin();
    };
    CHECK_MESSAGE(at("Pin project: alpha") == at("Open project: alpha") + 1, "each project's Open is followed by its Pin");
    test.command("projects.open:alpha");
    CHECK(test.app->ctx.screen() == "projects");
    CHECK(projects(test).shown() == "alpha");
    test.command("projects.pin:alpha");
    CHECK(test.app->ctx.shell.sidebar().pinned_projects().contains("alpha"));
    test.command("projects.bogus");
    CHECK(test.notice() == "Unknown command projects.bogus");
}

TEST_CASE("projects: a project without a folder keeps the current folder") {
    TestApp test;
    auto cwd = test.app->ctx.runtime.cwd();
    test.send(sidebar_intent(nav::OpenProject{"ghost"}));
    CHECK_MESSAGE(test.app->ctx.runtime.project() == "ghost", "a project is a group, not a folder");
    CHECK(test.app->ctx.runtime.cwd() == cwd);
    CHECK(test.app->ctx.screen() == "projects");
}

TEST_CASE("projects: picking a folder leaves the active project alone") {
    TestApp test;
    test.send(area(to::projects::Activate{"alpha"}));
    auto other = test.dir.path() / "other";
    fs::create_directories(other);
    other = fs::canonical(other);
    test.send(msg::FolderChosen{std::optional<fs::path>(other)});
    CHECK(test.app->ctx.runtime.cwd() == other);
    CHECK_MESSAGE(test.app->ctx.runtime.project() == "alpha", "a folder is not a project: picking one leaves the project alone");
}

TEST_CASE("projects: the New project screen makes a project whose folder links its sessions") {
    TestApp test;
    auto home = test.dir.path() / "launch";
    auto moved = test.dir.path() / "moved";
    fs::create_directories(home);
    fs::create_directories(moved);
    auto session_dir = test.app->ctx.runtime.cwd();
    auto session = chat_in(test, "alpha", "first");
    CHECK(session_project(*test.app, session) == "alpha");
    auto listed = [&](std::string_view name) { return std::ranges::contains(test.app->ctx.shell.sidebar().projects(), name); };
    CHECK_MESSAGE(listed("alpha"), "listed from the session's project");

    test.send(sidebar_intent(nav::NewProject{}));
    CHECK_MESSAGE(test.app->ctx.screen() == "projects", "the + opens the New project screen");
    CHECK(projects(test).form() != nullptr);
    test.send(form(create::local::Name{" Launch "}));
    test.send(form(create::local::Folder{home.string()}));
    test.send(form(create::local::Create{}));
    CHECK_MESSAGE(listed("Launch"), "an empty project is listed");
    CHECK_MESSAGE(test.app->ctx.runtime.project() == "Launch", "a new project becomes the active one");
    CHECK_MESSAGE(test.app->ctx.runtime.cwd() == home, "new chats start in its folder");
    CHECK_MESSAGE(session_project(*test.app, session) == "alpha", "making a project moves nothing");
    CHECK(linked(home).empty());

    test.send(sidebar_message(sidebar::msg::MoveTo{session, std::string("Launch")}));
    CHECK_MESSAGE(session_project(*test.app, session) == "Launch", "moved from its sidebar row");
    CHECK_MESSAGE(linked(home) == std::vector<fs::path>{session_dir}, ".omgui links the folder the session runs in");

    // Changing the folder moves the links and the active folder with it.
    test.send(area(pa::msg::FolderChosen{pa::pick::Project{"Launch"}, std::optional<fs::path>(moved)}));
    CHECK_MESSAGE(linked(home).empty(), "the old folder's links are gone");
    CHECK(linked(moved) == std::vector<fs::path>{session_dir});
    CHECK(test.app->ctx.runtime.cwd() == moved);

    // The composer's chip moves the shown session out of every project.
    test.send(sidebar_message(sidebar::msg::OpenSession{session}));
    test.send(msg::Composer{loa::ui::session::composer::msg::Pick{loa::ui::session::composer::PickerId::Project, 0}});
    CHECK_MESSAGE(session_project(*test.app, session) == std::nullopt, "moved out by the composer chip");
    CHECK_MESSAGE(linked(moved).empty(), "and its link goes with it");
}

TEST_CASE("projects: a project row starts a session in it and deleting keeps its sessions") {
    TestApp test;
    auto session = chat_in(test, "alpha", "first");
    test.app->ctx.shell.sidebar_mut().set_pinned_projects({"alpha"});

    test.send(sidebar_intent(nav::NewSessionIn{"alpha"}));
    auto configs = test.harness.seen().configs;
    REQUIRE_FALSE(configs.empty());
    CHECK_MESSAGE(configs.back().project == "alpha", "the row's + starts a session in it");

    test.send(sidebar_intent(nav::DeleteProject{"alpha"}));
    CHECK_MESSAGE(test.app->ctx.screen() == "projects", "the row's X asks on the project screen");
    CHECK_MESSAGE(session_project(*test.app, session) == "alpha", "nothing is deleted before the confirmation");
    test.send(local(screen::local::ConfirmDelete{}));

    CHECK_MESSAGE(session_project(*test.app, session) == std::nullopt, "its sessions are kept, in no project");
    CHECK(test.app->ctx.runtime.sessions().get(session) != nullptr);
    CHECK_FALSE(std::ranges::contains(test.app->ctx.shell.sidebar().projects(), std::string_view("alpha")));
    CHECK_MESSAGE(test.app->ctx.shell.sidebar().pinned_projects().empty(), "its pin goes");
    CHECK_MESSAGE(!std::ranges::contains(test.app->ctx.shell.open_projects(), std::string("alpha")), "its tab goes");
    CHECK_MESSAGE(test.app->ctx.runtime.project() == std::nullopt, "it is no longer the active project");
    CHECK_MESSAGE(test.app->ctx.screen() != "projects", "its screen closes");
}

TEST_CASE("projects: ideas notes are their own file and reach chats only when shared") {
    TestApp test;
    auto folder = test.dir.path() / "planner";
    fs::create_directories(folder);
    create_project(test, "Planner", folder);
    auto notes = notes_path(test, "Planner");
    auto new_chat_context = [&] {
        test.send(area(to::projects::NewChatIn{"Planner"}));
        return last_context(test);
    };

    test.send(local(screen::local::Ideas{"- try iroh relays\r\n- add a sidebar"}));
    test.send(local(screen::local::Save{}));
    CHECK_MESSAGE(read_file(notes) == "- try iroh relays\r\n- add a sidebar", "stored as typed, next to the settings");
    CHECK_FALSE(projects(test).screen()->is_dirty());
    CHECK_MESSAGE(!contains(new_chat_context(), "try iroh relays"), "not shared by default");

    test.send(local(screen::local::ShareIdeas{true}));
    test.send(local(screen::local::Save{}));
    auto context = new_chat_context();
    CHECK_MESSAGE((contains(context, "## Ideas and notes") && contains(context, "- try iroh relays\r\n- add a sidebar")), context);

    // Saving other settings leaves notes edited outside since alone.
    write_file(notes, "edited outside");
    test.send(local(screen::local::Instructions{"Use rustls."}));
    test.send(local(screen::local::Save{}));
    CHECK(read_file(notes) == "edited outside");

    test.reopen();
    test.send(area(to::projects::Open{"Planner"}));
    const auto* overview = projects(test).screen()->overview();
    REQUIRE(overview != nullptr);
    CHECK((overview->ideas == std::expected<std::string, std::string>("edited outside")));
    CHECK(overview->settings.ideas_in_context);
    test.send(local(screen::local::ConfirmDelete{}));
    CHECK_MESSAGE(!fs::exists(notes), "deleting the project deletes its notes");
}

TEST_CASE("projects: notes edited outside are never overwritten by an older page edit") {
    TestApp test;
    auto folder = test.dir.path() / "planner";
    fs::create_directories(folder);
    create_project(test, "Planner", folder);
    auto notes = notes_path(test, "Planner");
    auto shown = [&] { return projects(test).screen()->overview()->ideas; };

    test.send(local(screen::local::Ideas{"typed in Loa"}));
    write_file(notes, "edited outside");
    test.send(area(to::projects::Open{"Planner"}));
    CHECK_MESSAGE((shown() == std::expected<std::string, std::string>("edited outside")), "a reload in between sees the outside edit");
    test.send(local(screen::local::Save{}));
    CHECK_MESSAGE(read_file(notes) == "edited outside", "and the page doesn't overwrite it");
    CHECK_MESSAGE(projects(test).screen()->is_dirty(), "the page edit stays until reloaded");

    test.send(local(screen::local::ReloadIdeas{}));
    CHECK_MESSAGE(!projects(test).screen()->is_dirty(), "reloading takes the notes from the file");
    test.send(local(screen::local::Ideas{"- then typed in Loa\nedited outside"}));
    test.send(local(screen::local::Save{}));
    CHECK(read_file(notes) == "- then typed in Loa\nedited outside");
    test.send(local(screen::local::Ideas{"- then typed in Loa\n- and again\nedited outside"}));
    test.send(local(screen::local::Save{}));
    CHECK_MESSAGE(read_file(notes) == "- then typed in Loa\n- and again\nedited outside", "a save is the base of the next");
}

TEST_CASE("projects: unreadable notes leave the page and the rest of the context working") {
    TestApp test;
    auto folder = test.dir.path() / "planner";
    fs::create_directories(folder);
    create_project(test, "Planner", folder);
    auto notes = notes_path(test, "Planner");
    const std::string bytes{'\xff', '\xfe', 'h', '\0', 'i', '\0'};
    write_file(notes, bytes);
    test.send(area(to::projects::Open{"Planner"}));
    const auto* overview = projects(test).screen()->overview();
    REQUIRE_MESSAGE(overview != nullptr, "the page still loads");
    REQUIRE_FALSE(overview->ideas.has_value());
    CHECK(contains(overview->ideas.error(), "Planner.ideas.md"));

    test.send(local(screen::local::Instructions{"Use rustls."}));
    test.send(local(screen::local::ShareIdeas{true}));
    test.send(local(screen::local::Save{}));
    CHECK_MESSAGE(read_file(notes) == bytes, "the notes file is left as it is");
    test.send(area(to::projects::NewChatIn{"Planner"}));
    auto context = last_context(test);
    CHECK_MESSAGE((contains(context, "Use rustls.") && !contains(context, "## Ideas and notes")), context);
}

TEST_CASE("projects: deleting a terminal conversation removes it from omp storage only") {
    TestApp test;
    auto folder = test.dir.path() / "project";
    auto other = test.dir.path() / "other";
    fs::create_directories(folder);
    fs::create_directories(other);
    auto storage = test.dir.path() / "omp/sessions/saved";
    auto doomed = saved_omp_session(storage, "doomed", folder, "Old terminal chat");
    auto sidecar = fs::path(doomed).replace_extension("");
    fs::create_directories(sidecar);
    write_file(sidecar / "artifact.md", "notes");
    auto kept = saved_omp_session(storage, "kept", folder, "Kept terminal chat");
    create_project(test, "Project", folder);
    CHECK(listed_history(test) == std::vector<std::string>{"doomed", "kept"});

    test.send(local(screen::local::DeleteOmpHistory{"doomed"}));
    CHECK_MESSAGE(fs::is_regular_file(doomed), "nothing is deleted before the confirmation");
    auto deletion = test.update(local(screen::local::ConfirmDeleteOmpHistory{}));
    test.send(screen_intent(vs::intent::OpenOmpHistory{"kept"}));
    CHECK_MESSAGE(stored(test) == 0, "no row opens during a deletion");
    test.send(area(pa::msg::FolderChosen{pa::pick::Project{"Project"}, std::optional<fs::path>(other)}));
    CHECK_MESSAGE(contains(test.notice(), "Wait for the current project operation"), "the folder stays during a deletion");
    test.run(std::move(deletion));
    CHECK((!fs::exists(doomed) && !fs::exists(sidecar) && fs::is_regular_file(kept)));
    CHECK(listed_history(test) == std::vector<std::string>{"kept"});

    test.reopen();
    test.send(area(to::projects::Open{"Project"}));
    CHECK_MESSAGE(listed_history(test) == std::vector<std::string>{"kept"}, "a fresh scan doesn't bring it back");
}

TEST_CASE("projects: a deletion finishing under the New project form still drops its row") {
    TestApp test;
    auto folder = test.dir.path() / "project";
    fs::create_directories(folder);
    auto storage = test.dir.path() / "omp/sessions/saved";
    auto doomed = saved_omp_session(storage, "doomed", folder, "Old terminal chat");
    saved_omp_session(storage, "kept", folder, "Kept terminal chat");
    create_project(test, "Project", folder);
    CHECK(listed_history(test) == std::vector<std::string>{"doomed", "kept"});

    test.send(local(screen::local::DeleteOmpHistory{"doomed"}));
    auto deletion = test.update(local(screen::local::ConfirmDeleteOmpHistory{}));
    test.send(area(to::projects::New{}));
    test.run(std::move(deletion));
    test.send(area(pa::msg::Form{create::Message(vs::CreateIntent(vs::create_intent::Cancel{}))}));
    CHECK(projects(test).form() == nullptr);
    CHECK_FALSE(fs::exists(doomed));
    CHECK_MESSAGE(listed_history(test) == std::vector<std::string>{"kept"}, "the project screen under the form was listed again");
}

TEST_CASE("projects: a failed deletion is reported and the history listed again") {
    TestApp test;
    auto folder = test.dir.path() / "project";
    fs::create_directories(folder);
    auto storage = test.dir.path() / "omp/sessions/saved";
    auto gone = saved_omp_session(storage, "gone", folder, "Removed in a terminal");
    saved_omp_session(storage, "kept", folder, "Kept terminal chat");
    create_project(test, "Project", folder);
    fs::remove(gone);

    test.send(local(screen::local::DeleteOmpHistory{"gone"}));
    test.send(local(screen::local::ConfirmDeleteOmpHistory{}));
    CHECK_MESSAGE(contains(test.notice(), "Can't delete OMP terminal session"), test.notice());
    CHECK(listed_history(test) == std::vector<std::string>{"kept"});
    test.send(local(screen::local::DeleteOmpHistory{"kept"}));
    test.send(local(screen::local::ConfirmDeleteOmpHistory{}));
    CHECK_MESSAGE(listed_history(test).empty(), "the history is free again after a failure");
}
