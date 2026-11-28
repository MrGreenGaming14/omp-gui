// Port of the `FileSettingsStore`, ideas notes and `.omgui` link tests of `crates/loa-projects/src/tests.rs`.

#include <doctest/doctest.h>

#include <algorithm>
#include <filesystem>
#include <set>

#include "loa/projects/context.hpp"
#include "loa/projects/links.hpp"
#include "loa/projects/settings.hpp"
#include "loa/runtime/tempfile.hpp"
#include "testing.hpp"

using namespace loa::projects;
using namespace loa::projects::test;
using loa::settings::ProjectSettings;
namespace fs = std::filesystem;

namespace {

// Replaces `project`'s ideas notes with `text` over the file as it is now.
void write_ideas(const FileSettingsStore& store, std::string_view project, std::string_view text) {
    auto seen = store.load_ideas(project).value().file;
    REQUIRE(store.save_ideas(project, std::move(seen), text).has_value());
}

} // namespace

TEST_CASE("file_store_names_lists_projects_and_skips_strays") {
    auto dir = loa::runtime::TempDir::create().value();
    const auto projects = dir.path() / "projects";
    const FileSettingsStore store(projects);
    CHECK(store.names().value().empty());
    std::vector<std::string> saved{"my/proj", "with space", "a.b", ".", "héllo", "loa"};
    for (const auto& name : saved) REQUIRE(store.save(name, ProjectSettings{}).has_value());
    auto expected = saved;
    std::ranges::sort(expected);
    CHECK(store.names().value() == expected);
    // Strays: no `.json` suffix, a bad escape, a non-canonical escape, a directory.
    write_file(projects / "notes.txt", "x");
    write_file(projects / "bad%zz.json", "{}");
    write_file(projects / "my%2fproj.json", "{}");
    fs::create_directory(projects / "dir.json");
    CHECK(store.names().value() == expected);
    // The folder round-trips; removing forgets a project and tolerates a missing one.
    ProjectSettings settings;
    settings.folder = fs::path("/work/my-proj");
    REQUIRE(store.save("my/proj", settings).has_value());
    CHECK(FileSettingsStore(projects).load("my/proj").value() == settings);
    REQUIRE(store.remove("my/proj").has_value());
    REQUIRE(store.remove("my/proj").has_value());
    CHECK(store.load("my/proj").value() == ProjectSettings{});
}

TEST_CASE("file_store_keeps_each_projects_ideas_next_to_its_settings") {
    auto dir = loa::runtime::TempDir::create().value();
    const auto projects = dir.path() / "projects";
    const FileSettingsStore store(projects);
    CHECK(store.load_ideas("loa").value().text.empty());
    const std::string windows = "# Ideas\r\n\r\n- try iroh relays\r\n";
    REQUIRE(store.save("loa", ProjectSettings{}).has_value());
    write_ideas(store, "loa", windows);
    write_ideas(store, "my/proj", "- web only");
    CHECK(read_file(projects / "loa.ideas.md") == windows);
    CHECK(store.load_ideas("loa").value().text == windows);
    CHECK(store.load_ideas("my/proj").value().text == "- web only");
    CHECK(store.names().value() == std::vector<std::string>{"loa"});
    write_file(projects / "bad.ideas.md", "\xff\xfe");
    auto bad = store.load_ideas("bad");
    REQUIRE_FALSE(bad.has_value());
    CHECK(bad.error().to_string().find("bad.ideas.md") != std::string::npos);

    // The share toggle is off for settings written before it existed, and stays out of the file while off.
    write_file(projects / "old.json", R"({"instructions": "Use rustls."})");
    CHECK_FALSE(store.load("old").value().ideas_in_context);
    CHECK(read_file(projects / "loa.json").find("ideas_in_context") == std::string::npos);

    REQUIRE(store.remove("loa").has_value());
    CHECK_FALSE(fs::exists(projects / "loa.ideas.md"));
    CHECK_FALSE(fs::exists(projects / "loa.json"));
    CHECK(store.load_ideas("my/proj").value().text == "- web only");
    fs::create_directory(projects / "locked.ideas.md");
    auto locked = store.remove("locked");
    REQUIRE_FALSE(locked.has_value());
    CHECK(locked.error().to_string().find("locked.ideas.md") != std::string::npos);
}

TEST_CASE("saving_ideas_never_replaces_an_edit_made_outside_since_they_were_loaded") {
    auto dir = loa::runtime::TempDir::create().value();
    const FileSettingsStore store(dir.path());
    const auto path = dir.path() / "loa.ideas.md";
    auto missing = store.load_ideas("loa").value().file;
    write_file(path, "written outside");
    auto changed = store.save_ideas("loa", missing, "typed in Loa");
    REQUIRE_FALSE(changed.has_value());
    CHECK(changed.error().kind == ErrorKind::ChangedOnDisk);
    CHECK(changed.error().path == path);
    CHECK(read_file(path) == "written outside");

    auto seen = store.load_ideas("loa").value().file;
    auto saved = store.save_ideas("loa", seen, "first save").value();
    saved = store.save_ideas("loa", saved, "second save").value();
    CHECK(read_file(path) == "second save");
    CHECK(store.save_ideas("loa", seen, "stale").error().kind == ErrorKind::ChangedOnDisk);
    write_file(path, "edited outside again");
    CHECK(store.save_ideas("loa", saved, "stale").error().kind == ErrorKind::ChangedOnDisk);
    CHECK_FALSE(store.save_ideas("other", saved, "elsewhere").has_value());
    CHECK(read_file(path) == "edited outside again");
    CHECK_FALSE(fs::exists(dir.path() / "other.ideas.md"));
}

TEST_CASE("ideas_join_the_context_only_while_shared_and_count_against_the_budget") {
    Builder a("a", "loa", 100);
    a.turn("Set up the Iroh endpoint", "Bound it to port 31415.", 110);
    Builder b("b", "loa", 200);
    b.turn("Theme the sidebar", "Dark tokens done.", 210);
    b.digest("Themed the sidebar with dark tokens.");
    MemorySource source{&a, &b};
    auto dir = loa::runtime::TempDir::create().value();
    const FileSettingsStore store(dir.path());
    const ContextRequest request{"loa", std::nullopt, "", 4000, 1000};
    write_ideas(store, "loa", "- try iroh relays\n" + std::string(4 * 4000, 'x'));

    auto off = build_context(source, LocalOnly{}, store, request).value();
    CHECK_FALSE(off.ideas.has_value());
    CHECK(off.digests.size() == 1);
    CHECK(render_markdown(off).find("try iroh relays") == std::string::npos);

    ProjectSettings shared;
    shared.instructions = "Use rustls.";
    shared.ideas_in_context = true;
    REQUIRE(store.save("loa", shared).has_value());
    auto on = build_context(source, LocalOnly{}, store, request).value();
    CHECK(render_markdown(on).find("## Ideas and notes\n\nThe user's own development ideas and notes for this project.\n\n"
                                   "- try iroh relays\n") != std::string::npos);
    CHECK(on.digests.empty());

    write_ideas(store, "loa", " \r\n");
    auto blank = build_context(source, LocalOnly{}, store, request).value();
    CHECK_FALSE(blank.ideas.has_value());
    CHECK(blank.digests.size() == 1);

    write_file(dir.path() / "loa.ideas.md", "- \x93q\x94");
    auto unreadable = build_context(source, LocalOnly{}, store, request).value();
    CHECK_FALSE(unreadable.ideas.has_value());
    CHECK(unreadable.digests.size() == 1);
    CHECK(render_markdown(unreadable).find("Use rustls.") != std::string::npos);
}

namespace {

std::vector<std::string> names(const fs::path& folder) {
    std::vector<std::string> out;
    for (const auto& e : fs::directory_iterator(folder / links::LINKS_DIR)) out.push_back(e.path().filename().string());
    std::ranges::sort(out);
    return out;
}

std::set<fs::path> linked(const fs::path& folder) {
    std::set<fs::path> out;
    for (const auto& e : fs::directory_iterator(folder / links::LINKS_DIR))
        if (fs::is_symlink(e.symlink_status())) out.insert(fs::read_symlink(e.path()));
    return out;
}

} // namespace

TEST_CASE("links_follow_the_session_directories") {
    auto root = loa::runtime::TempDir::create().value();
    const auto project = root.path() / "project";
    const auto api = root.path() / "a/api", other_api = root.path() / "b/api", web = root.path() / "web";
    for (const auto* d : {&project, &api, &other_api, &web}) fs::create_directories(*d);
    const auto missing = root.path() / "gone";

    REQUIRE(links::sync(project, {project, api, other_api, missing}).has_value());
    CHECK(names(project) == std::vector<std::string>{".gitignore", "api", "api-2"});
    CHECK(read_file(project / links::LINKS_DIR / ".gitignore") == "*\n");
    const auto api_link = fs::read_symlink(project / links::LINKS_DIR / "api");
    CHECK((api_link == api || api_link == other_api));

    REQUIRE(links::sync(project, {api, web}).has_value());
    CHECK(names(project).size() == 3);
    const auto listed = names(project);
    CHECK(std::ranges::find(listed, "web") != listed.end());
    CHECK(linked(project) == std::set<fs::path>{api, web});

    REQUIRE(links::sync(project, {}).has_value());
    CHECK_FALSE(fs::exists(project / links::LINKS_DIR));
}

TEST_CASE("remove_keeps_the_users_own_files") {
    auto root = loa::runtime::TempDir::create().value();
    const auto project = root.path() / "project", api = root.path() / "api";
    fs::create_directories(project);
    fs::create_directories(api);
    REQUIRE(links::sync(project, {api}).has_value());
    write_file(project / links::LINKS_DIR / "notes.md", "mine");
    REQUIRE(links::remove(project).has_value());
    CHECK(names(project) == std::vector<std::string>{"notes.md"});
    CHECK(fs::is_directory(api));
}
