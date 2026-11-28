// Port of the `models/agents.rs` tests, plus the agent goldens.

#include <doctest/doctest.h>

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <sstream>
#include <string>
#include <tuple>
#include <vector>

#include "loa/runtime/tempfile.hpp"
#include "loa/settings/models/agents.hpp"

using namespace loa::settings::models;
using namespace loa::settings::models::agents;
namespace fs = std::filesystem;

namespace {

const std::string SCOUT =
    "---\nname: code-scout\ndescription: Read-only research specialist.\nmodel: anthropic/claude-haiku-5-5\nthinkingLevel: low\ntools:\n  - read\n  - grep\n---\n\nYou are Code Scout.\n- Do not modify files\n";
const std::string BUNDLED =
    "---\nname: plan\n# why: planning only\ndescription: \"Software architect: plans\"\nmodel:\n  - \"@slow\"\n  - anthropic/claude-opus-5-5\nspawns:\n  - explore\ntools: [read, grep]\noutput:\n  properties:\n    plan:\n      type: string\n  optionalProperties:\n    notes:\n      type: array\n---\nNo trailing newline";

std::string crlf(std::string_view s) {
    std::string out;
    for (char c : s) {
        if (c == '\n') {
            out += '\r';
        }
        out += c;
    }
    return out;
}

std::string slurp(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::ostringstream s;
    s << in.rdbuf();
    return s.str();
}

void spit(const fs::path& path, std::string_view text) {
    std::ofstream out(path, std::ios::binary | std::ios::trunc);
    out << text;
}

AgentDefinition must_parse(std::string_view text) {
    auto def = parse(text);
    REQUIRE(def);
    return std::move(*def);
}

AgentDefinition must_create(std::string_view name, std::string_view description) {
    auto def = AgentDefinition::create(name, description);
    REQUIRE(def);
    return std::move(*def);
}

using Strs = std::vector<std::string>;

bool has_field(const std::vector<FieldError>& errors, std::string_view field) {
    return std::ranges::any_of(errors, [&](const FieldError& e) { return e.field == field; });
}

std::size_t count(std::string_view s, std::string_view part) {
    std::size_t n = 0;
    for (auto at = s.find(part); at != std::string_view::npos; at = s.find(part, at + part.size())) {
        ++n;
    }
    return n;
}

const fs::path GOLDEN = fs::path(LOA_GOLDEN_DIR) / "settings";

} // namespace

TEST_CASE("agent round trips are byte identical") {
    for (const auto& text : {SCOUT, BUNDLED, crlf(SCOUT), crlf(BUNDLED)}) {
        CHECK(must_parse(text).to_text() == text);
    }
    const auto def = must_parse(BUNDLED);
    CHECK(def.model() == ModelSpec::chain({"@slow", "anthropic/claude-opus-5-5"}));
    CHECK(def.spawns() == Strs{"explore"});
    CHECK(def.tools() == Strs{"read", "grep"});
    CHECK(must_parse(SCOUT).model() == ModelSpec::one("anthropic/claude-haiku-5-5"));
}

TEST_CASE("agent parse rejects missing frontmatter") {
    CHECK_FALSE(parse("name: x\n"));
    CHECK_FALSE(parse("---\nname: x\n"));
}

TEST_CASE("agent edits keep unknown keys and comments") {
    auto def = must_parse(BUNDLED);
    REQUIRE(def.set_tools(Strs{"read", "bash"}));
    REQUIRE(def.set_thinking_level("high"));
    REQUIRE(def.set_spawns(std::nullopt));
    def.set_body("New body\n");
    const auto text = def.to_text();
    CHECK(text.find("# why: planning only") != std::string::npos);
    CHECK(text.find("optionalProperties:\n    notes:\n      type: array\n") != std::string::npos);
    const auto back = must_parse(text);
    CHECK(back.tools() == Strs{"read", "bash"});
    CHECK(back.thinking_level() == "high");
    CHECK_FALSE(back.spawns());
    CHECK(back.body() == "New body\n");
    CHECK(back.model() == def.model());
}

TEST_CASE("agent CRLF edits keep CRLF") {
    auto def = must_parse(crlf(SCOUT));
    REQUIRE(def.set_thinking_level("high"));
    const auto text = def.to_text();
    CHECK(text.find("thinkingLevel: high\r\n") != std::string::npos);
    CHECK(count(text, "\n") == count(text, "\r\n"));
}

TEST_CASE("new agent has name and description") {
    const auto def = must_create("helper", "Does things: well");
    const auto back = must_parse(def.to_text());
    CHECK(back.name() == "helper");
    CHECK(back.description() == "Does things: well");
    CHECK_FALSE(back.tools());
}

TEST_CASE("agent list reads user, project and disabled agents") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    const auto agent_dir = tmp->path() / "agent";
    const auto project = tmp->path() / "proj";
    const auto user = agent_dir / "agents";
    const auto proj = project / ".omp" / "agents";
    fs::create_directories(user);
    fs::create_directories(proj);
    spit(user / "code-scout.md", SCOUT);
    spit(user / "plan.md.disabled", BUNDLED);
    spit(user / "broken.md", "no frontmatter");
    spit(user / "notes.txt", "ignored");
    spit(proj / "alpha.md", "---\nname: alpha\ndescription: d\n---\n");

    const auto [files, broken] = list(agent_dir, project);
    std::vector<std::tuple<std::string, AgentSource, bool>> seen;
    for (const auto& f : files) {
        seen.emplace_back(std::string(f.definition.name()), f.source, f.enabled);
    }
    CHECK(seen == std::vector<std::tuple<std::string, AgentSource, bool>>{
                      {"alpha", AgentSource::Project, true},
                      {"code-scout", AgentSource::User, true},
                      {"plan", AgentSource::User, false},
                  });
    REQUIRE(broken.size() == 1);
    CHECK(broken[0].first.filename() == "broken.md");

    const auto none = list(tmp->path() / "missing", std::nullopt);
    CHECK(none.files.empty());
    CHECK(none.broken.empty());
}

TEST_CASE("agent validate cases") {
    const auto ok = must_parse(SCOUT);
    CHECK(validate(ok, {}, {}).empty());
    const Strs read_grep{"read", "grep"};
    CHECK(validate(ok, read_grep, {}).empty());
    const Strs read{"read"};
    const auto errs = validate(ok, read, {});
    REQUIRE(errs.size() == 1);
    CHECK(errs[0].field == "tools");

    auto def = must_create("bad name", "");
    REQUIRE(def.set_thinking_level("extreme"));
    REQUIRE(def.set_spawns(Strs{"bad name", "ghost"}));
    REQUIRE(def.set_model(ModelSpec::chain({"a", ""})));
    const Strs other{"other"};
    const auto errors = validate(def, {}, other);
    for (const auto* f : {"name", "description", "thinkingLevel", "spawns", "model"}) {
        CHECK_MESSAGE(has_field(errors, f), f);
    }
    CHECK(std::ranges::count_if(errors, [](const FieldError& e) { return e.field == "spawns"; }) == 2);

    for (const auto& name : {std::string(), std::string(".hidden"), std::string("a/b"), std::string("../x"), std::string(65, 'x')}) {
        const auto d = must_create(name, "d");
        CHECK_MESSAGE(has_field(validate(d, {}, {}), "name"), name);
    }
}

TEST_CASE("agent write refuses overwrite and unsafe names") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    const auto dir = tmp->path() / "agents";
    const auto def = must_parse(SCOUT);
    const auto path = write(dir, def, Overwrite::No);
    REQUIRE(path);
    CHECK(*path == dir / "code-scout.md");
    CHECK(slurp(*path) == SCOUT);
    auto again = write(dir, def, Overwrite::No);
    REQUIRE_FALSE(again);
    CHECK(again.error().kind == Error::Kind::AlreadyExists);
    REQUIRE(write(dir, def, Overwrite::Yes));

    const auto disabled = set_enabled(*path, false);
    REQUIRE(disabled);
    auto twin = write(dir, def, Overwrite::No);
    REQUIRE_FALSE(twin);
    CHECK(twin.error().kind == Error::Kind::AlreadyExists);
    CHECK(disabled->filename() == "code-scout.md.disabled");

    for (const auto* name : {"../x", "a/b", ".hidden", ""}) {
        const auto d = must_create(name, "d");
        auto r = write(dir, d, Overwrite::Yes);
        REQUIRE_FALSE(r);
        CHECK_MESSAGE(r.error().kind == Error::Kind::Invalid, name);
    }
    CHECK_FALSE(fs::exists(tmp->path() / "x.md"));
}

TEST_CASE("agent set_enabled round trip and remove") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    const auto path = write(tmp->path(), must_parse(SCOUT), Overwrite::No);
    REQUIRE(path);
    const auto off = set_enabled(*path, false);
    REQUIRE(off);
    CHECK_FALSE(fs::exists(*path));
    CHECK(fs::exists(*off));
    CHECK(set_enabled(*off, false) == *off);
    const auto on = set_enabled(*off, true);
    REQUIRE(on);
    CHECK(*on == *path);
    CHECK(fs::exists(*path));
    REQUIRE(agents::remove(*path));
    CHECK_FALSE(fs::exists(*path));
    CHECK_FALSE(agents::remove(*path));
}

TEST_CASE("agent goldens round-trip and edit byte-identically") {
    for (const auto* name : {"agent.md", "agent-crlf.md", "agent.edited.md", "agent-crlf.edited.md", "agent.new.md"}) {
        const auto text = slurp(GOLDEN / name);
        REQUIRE(!text.empty());
        CHECK_MESSAGE(must_parse(text).to_text() == text, name);
    }

    auto def = must_parse(slurp(GOLDEN / "agent.md"));
    REQUIRE(def.set_tools(Strs{"read", "bash"}));
    REQUIRE(def.set_thinking_level("high"));
    REQUIRE(def.set_spawns(std::nullopt));
    REQUIRE(def.set_model(ModelSpec::one("@slow")));
    REQUIRE(def.set_description("Plans: carefully"));
    def.set_body("\nNew body\n");
    CHECK(def.to_text() == slurp(GOLDEN / "agent.edited.md"));

    auto crlf_def = must_parse(slurp(GOLDEN / "agent-crlf.md"));
    REQUIRE(crlf_def.set_thinking_level("minimal"));
    REQUIRE(crlf_def.set_spawns(Strs{"plan"}));
    CHECK(crlf_def.to_text() == slurp(GOLDEN / "agent-crlf.edited.md"));

    auto created = must_create("helper", "Does things: well");
    REQUIRE(created.set_tools(Strs{"read"}));
    REQUIRE(created.set_model(ModelSpec::chain({"@fast", "vendor/model"})));
    created.set_body("Help.\n");
    CHECK(created.to_text() == slurp(GOLDEN / "agent.new.md"));
}
