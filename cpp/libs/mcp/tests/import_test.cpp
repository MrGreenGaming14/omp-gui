// The import readers against the Rust readers' output, and the `import_fixture_table` test.

#include <doctest/doctest.h>

#include <fstream>
#include <sstream>

#include "loa/mcp/import.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::mcp;
using namespace loa::mcp::import;
namespace json = loa::runtime::json;

namespace {

const std::filesystem::path GOLDEN = std::filesystem::path(LOA_GOLDEN_DIR) / "mcp";

std::string slurp(const std::filesystem::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::ostringstream out;
    out << in.rdbuf();
    return out.str();
}

void spit(const std::filesystem::path& path, std::string_view text) {
    std::ofstream out(path, std::ios::binary);
    out << text;
}

const char* app_name(App app) {
    switch (app) {
    case App::Claude: return "Claude";
    case App::Cursor: return "Cursor";
    case App::Codex: return "Codex";
    }
    return "?";
}

} // namespace

TEST_CASE("the five import sources read what the Rust readers read") {
    std::vector<std::pair<std::string, std::unique_ptr<ImportSource>>> files;
    files.emplace_back("home/.claude.json", std::make_unique<JsonServers>(App::Claude, GOLDEN / "home/.claude.json", Path("/work/app")));
    files.emplace_back("project/.mcp.json", std::make_unique<JsonServers>(App::Claude, GOLDEN / "project/.mcp.json"));
    files.emplace_back("home/.cursor/mcp.json", std::make_unique<JsonServers>(App::Cursor, GOLDEN / "home/.cursor/mcp.json"));
    files.emplace_back("project/.cursor/mcp.json", std::make_unique<JsonServers>(App::Cursor, GOLDEN / "project/.cursor/mcp.json"));
    files.emplace_back("home/.codex/config.toml", std::make_unique<CodexToml>(GOLDEN / "home/.codex/config.toml"));

    json::Array sources;
    std::vector<Found> all;
    for (const auto& [rel, file] : files) {
        auto found = file->read();
        REQUIRE_MESSAGE(found, found.error());
        json::Array entries;
        for (const auto& f : *found) {
            if (f.spec) entries.push_back(json::Object{{"name", f.name}, {"spec", f.spec->to_json()}});
            else entries.push_back(json::Object{{"name", f.name}, {"error", f.spec.error()}});
            all.push_back(f);
        }
        sources.push_back(json::Object{{"file", rel}, {"app", app_name(file->app())}, {"entries", std::move(entries)}});
    }
    json::Array plan;
    for (const auto& entry : plan_found({}, all).entries) {
        if (std::holds_alternative<outcome::Imported>(entry.outcome)) {
            plan.push_back(json::Object{{"name", entry.name}, {"outcome", "imported"}});
        } else if (const auto* duplicate = std::get_if<outcome::Duplicate>(&entry.outcome)) {
            plan.push_back(json::Object{{"name", entry.name}, {"outcome", "duplicate"}, {"existing", duplicate->existing}});
        } else {
            plan.push_back(json::Object{{"name", entry.name}, {"outcome", "unsupported"}});
        }
    }
    const json::Value doc = json::Object{{"sources", std::move(sources)}, {"plan", std::move(plan)}};
    CHECK(json::dump_pretty(doc) + "\n" == slurp(GOLDEN / "import.expected.json"));
}

TEST_CASE("defaults lists the usual config locations") {
    auto without = defaults("/h");
    REQUIRE(without.size() == 3);
    CHECK(without[0]->path() == Path("/h/.claude.json"));
    CHECK(without[1]->path() == Path("/h/.cursor/mcp.json"));
    CHECK(without[2]->path() == Path("/h/.codex/config.toml"));
    CHECK(without[2]->app() == App::Codex);
    auto with = defaults("/h", Path("/p"));
    REQUIRE(with.size() == 5);
    CHECK(with[1]->path() == Path("/p/.mcp.json"));
    CHECK(with[1]->app() == App::Claude);
    CHECK(with[3]->path() == Path("/p/.cursor/mcp.json"));
    CHECK(with[3]->app() == App::Cursor);
}

TEST_CASE("import fixture table") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto& d = dir->path();
    spit(d / "c.json", R"({"mcpServers": {
        "a": {"command": "srv", "args": ["-v"], "env": {"N": 1}},
        "b": {"type": "http", "url": "https://x.test/mcp", "headers": {"K": "v"}, "disabled": true},
        "c": {"type": "sse", "url": "https://x.test/sse"},
        "d": {"envFile": ".env", "command": "x"},
        "e": {"args": []}
    }, "projects": {"/p": {"mcpServers": {"f": {"command": "local"}}}}})");
    spit(d / "c.toml",
         "[mcp_servers.g]\nurl = \"https://y.test\"\nbearer_token = \"t\"\nenabled = false\n[mcp_servers.h]\ncommand = \"x\"\nbearer_token_env_var = \"T\"\n");
    spit(d / "bad.json", "{");
    std::vector<std::unique_ptr<ImportSource>> files;
    files.push_back(std::make_unique<JsonServers>(App::Claude, d / "c.json", Path("/p")));
    files.push_back(std::make_unique<CodexToml>(d / "c.toml"));
    files.push_back(std::make_unique<JsonServers>(App::Cursor, d / "bad.json"));
    files.push_back(std::make_unique<CodexToml>(d / "missing.toml"));
    auto [found, errors] = scan(files);
    CHECK(errors.size() == 1);
    std::vector<std::pair<std::string, bool>> names;
    for (const auto& f : found) names.emplace_back(f.name, f.spec.has_value());
    const std::vector<std::pair<std::string, bool>> expect = {{"a", true},  {"b", true},  {"c", false}, {"d", false},
                                                              {"e", false}, {"f", true},  {"g", true},  {"h", false}};
    CHECK(names == expect);
    auto spec = [&](std::string_view name) { return *std::ranges::find(found, name, &Found::name)->spec; };
    const auto a = std::get<StdioSpec>(spec("a").transport);
    CHECK(a.args == std::vector<std::string>{"-v"});
    CHECK(a.env.at("N") == "1");
    CHECK(!spec("b").enabled);
    CHECK(!spec("g").enabled);
    CHECK(std::get<HttpSpec>(spec("g").transport).headers.at("Authorization") == "Bearer t");
    auto report = plan_found(std::vector{spec("a")}, found);
    CHECK(report.entries[0].outcome == Outcome(outcome::Duplicate{"a"}));
    CHECK(report.imported().size() == 3);
}
