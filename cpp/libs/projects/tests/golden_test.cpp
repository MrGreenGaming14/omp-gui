// Byte-exact checks against `cpp/tests/golden/projects/*`, written by `cpp/libs/projects/tools/golden_gen` (at ecc0cf0) from
// the Rust crate: project file names, settings JSON, ideas Markdown, `project_read`/`project_search` pages (with
// `x-own_patches` digests), tool schemas, the digest prompt, the context markdown and `.omgui` links.

#include <doctest/doctest.h>

#include <algorithm>
#include <filesystem>
#include <set>

#include "loa/projects/context.hpp"
#include "loa/projects/digester.hpp"
#include "loa/projects/links.hpp"
#include "loa/projects/settings.hpp"
#include "loa/projects/text.hpp"
#include "loa/projects/tools.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/tempfile.hpp"
#include "testing.hpp"

using namespace loa::projects;
using namespace loa::projects::test;
using loa::settings::DigestContent;
using loa::settings::FileNote;
using loa::settings::PinnedFile;
using loa::settings::ProjectSettings;
namespace fs = std::filesystem;
namespace json = loa::runtime::json;

namespace {

fs::path golden(std::string_view name) { return fs::path(LOA_GOLDEN_DIR) / "projects" / name; }

ProjectSettings full_settings() {
    ProjectSettings s;
    s.instructions = "Use rustls.\nNo \"unsafe\".";
    s.pinned = {PinnedFile{"src/lib.rs", "entry point"}, PinnedFile{"Cargo.toml", std::nullopt}};
    s.digest_model = "smol";
    s.folder = fs::path("/work/loa ✓");
    s.ideas_in_context = true;
    return s;
}

// The generator's fixture: `tests.rs`'s a, b, c plus `d` with a full digest written twice.
struct Fixture {
    Builder a{"a", "loa", 100};
    Builder b{"b", "loa", 200};
    Builder c{"c", "web", 300};
    Builder d{"d", "loa", 50};
    std::shared_ptr<MemorySource> source;

    Fixture() {
        auto t = a.turn("Set up the Iroh endpoint", "Bound it to port 31415.", 110);
        a.call(t, "bash", "cargo test -p net", "ok: 12 passed");
        a.commit(Draft::insert(a.root, K::Todo, attrs({{attr::STATE, Value("pending")}}), "write docs"));
        b.turn("Theme the sidebar", "Dark tokens done.", 210);
        b.digest("Themed the sidebar with dark tokens.");
        c.turn("iroh elsewhere", "no", 310);
        auto t1 = d.turn("Write the relay docs", "Wrote docs/relay.md with a \"quoted\" note.", 60);
        d.turn("héllo wörld ✓", "multi\nline\n  reply", 70);
        d.digest("first");
        DigestContent full;
        full.summary = "Documented the relay.\nTwo lines.";
        full.decisions = {"Use a self hosted relay", "Keep port 31415"};
        full.files = {FileNote{"docs/relay.md", "written"}, FileNote{"README.md", ""}};
        full.open_items = {"link from the README"};
        full.pointers = {t1};
        d.write_digest(full, "smol");
        source = std::make_shared<MemorySource>(std::initializer_list<const Builder*>{&a, &b, &c, &d});
    }
};

} // namespace

TEST_CASE("golden: project file names match Rust's percent encoding") {
    const auto rows = read_file(golden("filenames.tsv"));
    std::size_t count = 0;
    for (auto line : text::lines(rows)) {
        const auto tab = line.rfind('\t');
        REQUIRE(tab != std::string_view::npos);
        const auto name = line.substr(0, tab), file = line.substr(tab + 1);
        INFO(name);
        CHECK(encode_name(name) + ".json" == file);
        CHECK(FileSettingsStore("/d").path(name).filename() == file);
        CHECK(decode_name(file.substr(0, file.size() - 5)) == std::optional<std::string>(std::string(name)));
        ++count;
    }
    CHECK(count == 11);
}

TEST_CASE("golden: project settings JSON round-trips byte for byte") {
    for (const auto& [file, settings] : {std::pair{"settings.full.json", full_settings()}, std::pair{"settings.default.json", ProjectSettings{}}}) {
        INFO(file);
        const auto bytes = read_file(golden(file));
        CHECK(json::dump_pretty(settings_to_json(settings)) == bytes);
        auto parsed = settings_from_json(json::parse(bytes).value());
        REQUIRE(parsed.has_value());
        CHECK(*parsed == settings);
        CHECK(json::dump_pretty(settings_to_json(*parsed)) == bytes);
        // Through the store, as the app writes it.
        auto dir = loa::runtime::TempDir::create().value();
        const FileSettingsStore store(dir.path());
        REQUIRE(store.save("p", settings).has_value());
        CHECK(read_file(dir.path() / "p.json") == bytes);
    }
}

TEST_CASE("golden: ideas notes are stored byte for byte") {
    const auto bytes = read_file(golden("ideas.md"));
    auto dir = loa::runtime::TempDir::create().value();
    const FileSettingsStore store(dir.path());
    auto seen = store.load_ideas("full").value().file;
    REQUIRE(store.save_ideas("full", std::move(seen), bytes).has_value());
    CHECK(read_file(dir.path() / "full.ideas.md") == bytes);
    CHECK(store.load_ideas("full").value().text == bytes);
}

TEST_CASE("golden: project_read and project_search pages match Rust") {
    Fixture f;
    loa::runtime::Executor exec(1);
    auto tools = tool_set(f.source, std::make_shared<LocalOnly>());
    for (const auto& spec : tools->specs()) {
        INFO(spec.name);
        CHECK(spec.input_schema == read_file(golden(spec.name + ".schema.json")));
    }
    const std::tuple<const char*, const char*, const char*> calls[] = {
        {"read-a.txt", "project_read", R"({"session":"a"})"},
        {"read-a-turn-64.txt", "project_read", R"({"session":"a","node":2,"max_bytes":64})"},
        {"read-a-turn-64-next.txt", "project_read", R"({"session":"a","node":2,"max_bytes":64,"offset":63})"},
        {"read-b.txt", "project_read", R"({"session":"b"})"},
        {"read-d.txt", "project_read", R"({"session":"d"})"},
        {"search-iroh.json", "project_search", R"({"query":"iroh"})"},
        {"search-all-words.json", "project_search", R"({"query":"31415 bound","mode":"all_words"})"},
        {"search-call.json", "project_search", R"({"query":"12 passed","element":"call"})"},
        {"search-attr.json", "project_search", R"({"attributes":{"tool":"BA","role":null}})"},
        {"search-tool-attr.json", "project_search", R"({"attributes":{"tool":"BA"}})"},
        {"search-messages-page.json", "project_search", R"({"element":"message","limit":3})"},
        {"search-messages-page2.json", "project_search", R"({"element":"message","limit":3,"cursor":"3"})"},
        {"search-digest.json", "project_search", R"({"query":"relay","element":"digest","sessions":["d","b"]})"},
        {"search-unicode.json", "project_search", R"({"query":"WÖRLD"})"},
        {"search-own.json", "project_search", R"({"query":"dark tokens","sessions":["b"]})"},
    };
    for (const auto& [file, tool, args] : calls) {
        INFO(file);
        auto out = block_on(exec, tools->call("b", tool, args));
        REQUIRE(out.has_value());
        CHECK(*out == read_file(golden(file)));
    }
}

TEST_CASE("golden: x-own_patches digests read back as Rust wrote them") {
    Fixture f;
    const auto dom = f.d.dom();
    auto digest = digest_of(dom).value();
    CHECK(digest.model == std::optional<std::string>("smol"));
    CHECK(digest.source_seq == 8);
    CHECK_FALSE(digest.stale);
    CHECK(dom.get(digest.node)->int_attr(OWN_PATCHES) == 2);
    // Rewriting from the decoded content gives the same element again.
    Builder again("d", "loa", 50);
    auto t1 = again.turn("Write the relay docs", "Wrote docs/relay.md with a \"quoted\" note.", 60);
    again.turn("héllo wörld ✓", "multi\nline\n  reply", 70);
    again.digest("first");
    CHECK(digest.content.pointers == std::vector<NodeId>{t1});
    again.write_digest(digest.content, "smol");
    CHECK(again.dom() == dom);
}

TEST_CASE("golden: digest prompt and project context markdown match Rust") {
    Fixture f;
    CHECK(build_prompt(f.a.dom(), PromptLimits{}).value().text == read_file(golden("prompt-a.txt")));
    auto dir = loa::runtime::TempDir::create().value();
    const FileSettingsStore store(dir.path());
    ProjectSettings settings;
    settings.instructions = "Use rustls.";
    settings.pinned = full_settings().pinned;
    settings.ideas_in_context = true;
    REQUIRE(store.save("loa", settings).has_value());
    auto seen = store.load_ideas("loa").value().file;
    REQUIRE(store.save_ideas("loa", std::move(seen), "- try iroh relays\n").has_value());
    auto packed = build_context(*f.source, LocalOnly{}, store, ContextRequest{"loa", "a", "relay docs", 4000, 1000}).value();
    CHECK(render_markdown(packed) == read_file(golden("context.md")));
}

TEST_CASE("golden: .omgui links match Rust's names and targets") {
    auto scratch = loa::runtime::TempDir::create().value();
    const auto& root = scratch.path();
    const auto project = root / "project";
    std::set<fs::path> wanted;
    for (const char* d : {"project", "a/api", "b/api", "web", "root/x y"}) {
        fs::create_directories(root / d);
        wanted.insert(root / d);
    }
    wanted.insert(root / "gone");
    REQUIRE(links::sync(project, wanted).has_value());
    std::vector<std::string> lines;
    for (const auto& e : fs::directory_iterator(project / links::LINKS_DIR)) {
        const auto name = e.path().filename().string();
        if (fs::is_symlink(e.symlink_status()))
            lines.push_back(name + " -> " + fs::read_symlink(e.path()).lexically_relative(root).string());
        else
            lines.push_back(name + " = " + text::debug_str(read_file(e.path())));
    }
    std::ranges::sort(lines);
    std::string got;
    for (const auto& l : lines) got += l + "\n";
    CHECK(got == read_file(golden("links.txt")));
}
