// Port of the `models/workflows.rs` tests (the private `parse`/`render` ones run through
// `load`/`save`), plus the workflow goldens. The `BETWEEN_WRITES` hook test is not
// ported: it needs a test-only hook inside `commit`.

#include <doctest/doctest.h>

#include <filesystem>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

#include <asio/co_spawn.hpp>
#include <asio/io_context.hpp>
#include <asio/use_future.hpp>

#include "loa/runtime/tempfile.hpp"
#include "loa/settings/models/workflows.hpp"

using namespace loa::settings::models;
using namespace loa::settings::models::workflows;
using loa::settings::models::selector::Selector;
namespace fs = std::filesystem;

namespace {

const std::string START = "<!-- omp-gui:workflow ";
const std::string END = "<!-- /omp-gui:workflow -->";
const std::string ALLOWED = "- Allowed agents: ";
const std::string FIELD = "<!-- loa:workflow-field -->";
const std::string NOTES_FIELD = "<!-- loa:workflow-notes -->";
const std::string CONFIG = "modelRoles:\n  default: vendor/default\n  DEEP: vendor/model:high\n  OTHER: vendor/other\n";

class Accept final : public SettingsValidator {
  public:
    loa::contracts::Awaitable<Result<>> check_config(std::string, std::vector<std::pair<std::string, loa::runtime::json::Value>>) const override {
        co_return Result<>{};
    }
    loa::contracts::Awaitable<Result<>> check_models(std::string) const override { co_return Result<>{}; }
};

class Reject final : public SettingsValidator {
  public:
    loa::contracts::Awaitable<Result<>> check_config(std::string, std::vector<std::pair<std::string, loa::runtime::json::Value>>) const override {
        co_return std::unexpected(Error::rejected("config.yml", "rejected"));
    }
    loa::contracts::Awaitable<Result<>> check_models(std::string) const override { co_return Result<>{}; }
};

const Accept ACCEPT;

Result<> run(loa::contracts::Awaitable<Result<>> task) {
    asio::io_context io;
    auto f = asio::co_spawn(io, std::move(task), asio::use_future);
    io.run();
    return f.get();
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

std::string replace(std::string s, std::string_view from, std::string_view to) {
    for (auto at = s.find(from); at != std::string::npos; at = s.find(from, at + to.size())) {
        s.replace(at, from.size(), to);
    }
    return s;
}

std::string with_eol(std::string_view s, std::string_view eol) { return replace(std::string(s), "\n", eol); }

bool contains(const std::string& s, std::string_view part) { return s.find(part) != std::string::npos; }

std::size_t count(std::string_view s, std::string_view part) {
    std::size_t n = 0;
    for (auto at = s.find(part); at != std::string_view::npos; at = s.find(part, at + part.size())) {
        ++n;
    }
    return n;
}

struct Setup {
    loa::runtime::TempDir dir;
    WorkflowFiles files;
};

Setup setup(std::string_view rules) {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto files = WorkflowFiles::in_agent_dir(dir->path());
    spit(files.config, CONFIG);
    spit(files.rules, rules);
    return {std::move(*dir), std::move(files)};
}

std::pair<std::vector<Workflow>, WorkflowSnapshot> must_load(const WorkflowFiles& files) {
    auto loaded = files.load();
    REQUIRE(loaded);
    return std::move(*loaded);
}

Workflow find(std::vector<Workflow> list, std::string_view role) {
    for (auto& w : list) {
        if (w.role == role) {
            return std::move(w);
        }
    }
    FAIL("missing role " << role);
    return {};
}

std::pair<Workflow, WorkflowSnapshot> deep(const WorkflowFiles& files) {
    auto [list, snapshot] = must_load(files);
    return {find(std::move(list), "DEEP"), std::move(snapshot)};
}

Workflow managed(std::string role) { return {std::move(role), {}, Rules::managed("Research", {"scout"}, {}, "Cite sources.")}; }

// What `render` writes for a new managed workflow: create it in an empty RULES.md.
std::string rendered(const Workflow& workflow) {
    auto s = setup("");
    auto [_, snapshot] = must_load(s.files);
    REQUIRE(run(s.files.save(workflow, std::nullopt, snapshot, ACCEPT)));
    auto text = slurp(s.files.rules);
    REQUIRE(text.ends_with(END + "\n"));
    text.pop_back();
    return text;
}

// The rules a RULES.md text loads as for `role` (with `role` in the config).
Result<Rules> rules_of(std::string_view text, std::string_view role) {
    auto s = setup(text);
    spit(s.files.config, CONFIG + "  " + std::string(role) + ": vendor/x\n");
    auto loaded = s.files.load();
    if (!loaded) {
        return std::unexpected(loaded.error());
    }
    return find(std::move(loaded->first), role).rules;
}

} // namespace

TEST_CASE("hand-written edit preserves outside bytes, LF and CRLF") {
    for (std::string eol : {"\n", "\r\n"}) {
        const auto prefix = with_eol("# Rules\n\nBefore\n\n", eol);
        const auto section = with_eol("## Deep Mode\n\nWhen the active model role is `DEEP`, follow this.\n\n### Nested\ntext\n\n", eol);
        const auto suffix = with_eol("## Other\n\nAfter\n", eol);
        auto s = setup(prefix + section + suffix);
        auto [workflow, snapshot] = deep(s.files);
        const auto replacement = replace(section, "text", "changed");
        workflow.rules = Rules::hand_written(replacement);
        REQUIRE(run(s.files.save(workflow, "DEEP", snapshot, ACCEPT)));
        CHECK(slurp(s.files.rules) == prefix + replacement + suffix);
    }
}

TEST_CASE("hand-written rename only changes the section and keeps models") {
    const std::string original =
        "outside `DEEP`\n\n## Deep Mode\nWhen the active model role is `DEEP`, mention `DEEP`.\n\n## Other\noutside `DEEP`\n";
    auto s = setup(original);
    auto [workflow, snapshot] = deep(s.files);
    workflow.role = "STUDY";
    REQUIRE(run(s.files.save(workflow, "DEEP", snapshot, ACCEPT)));
    CHECK(slurp(s.files.rules) ==
          "outside `DEEP`\n\n## Deep Mode\nWhen the active model role is `STUDY`, mention `STUDY`.\n\n## Other\noutside `DEEP`\n");
    auto [list, _] = must_load(s.files);
    CHECK(find(list, "STUDY").models == std::vector{*Selector::parse("vendor/model:high")});
}

TEST_CASE("hand-written delete removes the exact section") {
    for (std::string eol : {"\n", "\r\n"}) {
        const auto prefix = with_eol("# Rules\n\nbefore\n\n", eol);
        const auto section = with_eol("## Deep Mode\nWhen the active model role is `DEEP`, act.\n\n", eol);
        const auto suffix = with_eol("## Other\nafter\n", eol);
        auto s = setup(prefix + section + suffix);
        auto [_, snapshot] = deep(s.files);
        REQUIRE(run(s.files.remove("DEEP", snapshot, ACCEPT)));
        CHECK(slurp(s.files.rules) == prefix + suffix);
        CHECK_FALSE(contains(slurp(s.files.config), "  DEEP:"));
    }
}

TEST_CASE("changed files refuse save, rename and delete") {
    for (bool change_config : {false, true}) {
        auto s = setup("## Deep Mode\nWhen the active model role is `DEEP`, act.\n");
        auto [workflow, snapshot] = deep(s.files);
        const auto& path = change_config ? s.files.config : s.files.rules;
        const auto changed = slurp(path) + "# external\n";
        spit(path, changed);
        auto r = run(s.files.save(workflow, "DEEP", snapshot, ACCEPT));
        REQUIRE_FALSE(r);
        CHECK(r.error().kind == Error::Kind::ChangedOnDisk);
        workflow.role = "STUDY";
        r = run(s.files.save(workflow, "DEEP", snapshot, ACCEPT));
        REQUIRE_FALSE(r);
        CHECK(r.error().kind == Error::Kind::ChangedOnDisk);
        r = run(s.files.remove("DEEP", snapshot, ACCEPT));
        REQUIRE_FALSE(r);
        CHECK(r.error().kind == Error::Kind::ChangedOnDisk);
        CHECK(slurp(path) == changed);
    }
}

TEST_CASE("managed create, update, rename and delete round-trip") {
    const std::string original = "# Rules\n\nUser text.\n\n";
    auto s = setup(original);
    {
        auto [_, snapshot] = must_load(s.files);
        REQUIRE(run(s.files.save(managed("RESEARCH"), std::nullopt, snapshot, ACCEPT)));
    }
    auto [list, snapshot] = must_load(s.files);
    auto item = find(std::move(list), "RESEARCH");
    CHECK(item.models == std::vector{Selector::model("vendor/default")});
    item.rules.notes = "Changed";
    REQUIRE(run(s.files.save(item, "RESEARCH", snapshot, ACCEPT)));
    {
        auto [_, snap] = must_load(s.files);
        item.role = "STUDY";
        REQUIRE(run(s.files.save(item, "RESEARCH", snap, ACCEPT)));
    }
    {
        auto [_, snap] = must_load(s.files);
        REQUIRE(run(s.files.remove("STUDY", snap, ACCEPT)));
    }
    CHECK(slurp(s.files.rules) == original);
    CHECK(slurp(s.files.config) == CONFIG);
}

TEST_CASE("unsafe markers, collisions and flow maps are refused") {
    for (const auto* rules : {"<!-- omp-gui:workflow X -->\n", "<!-- /omp-gui:workflow -->\n", "<!-- omp-gui:workflow -->\n"}) {
        auto s = setup(rules);
        CHECK_FALSE(s.files.load());
    }
    auto s = setup("");
    {
        auto [_, snapshot] = must_load(s.files);
        for (const auto* role : {"default", "Default", "deep", "OTHER"}) {
            CHECK_FALSE(run(s.files.save(managed(role), std::nullopt, snapshot, ACCEPT)));
        }
    }
    spit(s.files.config, "modelRoles: {default: vendor/default}\n");
    auto [_, snapshot] = must_load(s.files);
    CHECK_FALSE(run(s.files.save(managed("NEW"), std::nullopt, snapshot, ACCEPT)));
    CHECK(slurp(s.files.config) == "modelRoles: {default: vendor/default}\n");
}

TEST_CASE("rejected validation does not write files") {
    auto s = setup("# Rules\n");
    auto [_, snapshot] = must_load(s.files);
    const Reject reject;
    CHECK_FALSE(run(s.files.save(managed("NEW"), std::nullopt, snapshot, reject)));
    CHECK(slurp(s.files.config) == CONFIG);
    CHECK(slurp(s.files.rules) == "# Rules\n");
}

TEST_CASE("creation requires default and rejects invalid rules") {
    auto s = setup("# Rules\n");
    spit(s.files.config, "modelRoles:\n  OTHER: vendor/model\n");
    {
        auto [_, snapshot] = must_load(s.files);
        CHECK_FALSE(run(s.files.save(managed("NEW"), std::nullopt, snapshot, ACCEPT)));
    }
    spit(s.files.config, CONFIG);
    auto [_, snapshot] = must_load(s.files);
    auto item = managed("NEW");
    item.rules.notes = "<!-- /omp-gui:workflow -->";
    CHECK_FALSE(run(s.files.save(item, std::nullopt, snapshot, ACCEPT)));
    CHECK(slurp(s.files.config) == CONFIG);
    CHECK(slurp(s.files.rules) == "# Rules\n");
}

TEST_CASE("marker mismatches and duplicates are refused") {
    const auto valid = rendered(managed("NEW"));
    for (const auto& rules : {
             replace(valid, "<!-- omp-gui:workflow NEW", "<!-- omp-gui:workflow WRONG"),
             valid + "\n" + valid + "\n",
             replace(valid, END, "<!-- /omp-gui:workflow bad -->"),
             "<!-- omp-gui:workflow OUTER -->\n" + valid + "\n" + END,
         }) {
        CHECK_FALSE(rules_of(rules, "NEW"));
    }
}

TEST_CASE("role_for maps names like Rust") {
    CHECK(role_for("Deep Research") == "DEEP_RESEARCH");
    CHECK(role_for("  my  weird!!name-1 ") == "MY_WEIRD_NAME-1");
    CHECK(role_for("_lead_ trail_") == "LEAD__TRAIL");
    CHECK(role_for("café au lait") == "CAF_AU_LAIT");
    CHECK_FALSE(role_for("!!!"));
    CHECK_FALSE(role_for(""));
}

TEST_CASE("role without a section is editable and deletable") {
    auto s = setup("# Rules\n");
    auto [item, snapshot] = deep(s.files);
    CHECK(item.rules == Rules::none());
    item.models = {Selector::model("vendor/changed")};
    REQUIRE(run(s.files.save(item, "DEEP", snapshot, ACCEPT)));
    auto [_, snap] = must_load(s.files);
    REQUIRE(run(s.files.remove("DEEP", snap, ACCEPT)));
    CHECK(slurp(s.files.rules) == "# Rules\n");
}

TEST_CASE("managed CRLF and user blank lines survive a round trip") {
    const std::string original = "# Rules\r\n\r\n\r\nkeep\r\n\r\n\r\n";
    auto s = setup(original);
    {
        auto [_, snapshot] = must_load(s.files);
        REQUIRE(run(s.files.save(managed("NEW"), std::nullopt, snapshot, ACCEPT)));
    }
    const auto text = slurp(s.files.rules);
    CHECK(text.starts_with(original));
    CHECK(count(text, "\n") == count(text, "\r\n"));
    auto [_, snapshot] = must_load(s.files);
    REQUIRE(run(s.files.remove("NEW", snapshot, ACCEPT)));
    CHECK(slurp(s.files.rules) == original);
}

TEST_CASE("deleting the middle managed section preserves its neighbours") {
    auto s = setup("# Rules\n");
    for (const auto* role : {"FIRST", "SECOND"}) {
        auto [_, snapshot] = must_load(s.files);
        REQUIRE(run(s.files.save(managed(role), std::nullopt, snapshot, ACCEPT)));
    }
    auto [_, snapshot] = must_load(s.files);
    REQUIRE(run(s.files.remove("FIRST", snapshot, ACCEPT)));
    const auto after = slurp(s.files.rules);
    CHECK(after.starts_with("# Rules\n"));
    CHECK_FALSE(contains(after, "workflow FIRST"));
    CHECK(contains(after, "workflow SECOND"));
}

TEST_CASE("an unrelated section cannot be converted to managed") {
    const std::string text = "## Deep Mode\nWhen the active model role is `DEEP`, act.\n";
    auto s = setup(text);
    auto [item, snapshot] = deep(s.files);
    item.rules = managed("DEEP").rules;
    CHECK_FALSE(run(s.files.save(item, "DEEP", snapshot, ACCEPT)));
    CHECK(slurp(s.files.rules) == text);
}

TEST_CASE("a raised heading cannot swallow the following sections") {
    const std::string original =
        "## Deep Mode\nWhen the active model role is `DEEP`, act.\n\n## Other Mode\nWhen the active model role is `OTHER`, other.\n";
    auto s = setup(original);
    auto [item, snapshot] = deep(s.files);
    item.rules = Rules::hand_written("# Deep Mode\nWhen the active model role is `DEEP`, act.\n\n");
    CHECK_FALSE(run(s.files.save(item, "DEEP", snapshot, ACCEPT)));
    CHECK(slurp(s.files.rules) == original);
}

TEST_CASE("the edited section must keep its exact span in the whole file") {
    const std::string original = "## Deep Mode\nWhen the active model role is `DEEP`, act.\n\n## Notes\nUser notes.\n";
    auto s = setup(original);
    auto [item, snapshot] = deep(s.files);
    item.rules = Rules::hand_written("# Deep Mode\nWhen the active model role is `DEEP`, act.\n\n");
    CHECK_FALSE(run(s.files.save(item, "DEEP", snapshot, ACCEPT)));
    CHECK(slurp(s.files.rules) == original);
}

TEST_CASE("a dropped trailing newline keeps the following section") {
    for (std::string eol : {"\n", "\r\n"}) {
        const auto other = with_eol("## Other Mode\nWhen the active model role is `OTHER`, other.\n", eol);
        auto s = setup(with_eol("## Deep Mode\nWhen the active model role is `DEEP`, act.\n\n", eol) + other);
        auto [item, snapshot] = deep(s.files);
        item.rules = Rules::hand_written(with_eol("## Deep Mode\nWhen the active model role is `DEEP`, changed.", eol));
        REQUIRE(run(s.files.save(item, "DEEP", snapshot, ACCEPT)));
        CHECK(slurp(s.files.rules) == with_eol("## Deep Mode\nWhen the active model role is `DEEP`, changed.\n", eol) + other);
        auto [list, _] = must_load(s.files);
        CHECK(find(std::move(list), "OTHER").rules == Rules::hand_written(other));
    }
}

TEST_CASE("the edited section takes the file's line endings") {
    auto s = setup("# Rules\r\n\r\n## Deep Mode\r\nWhen the active model role is `DEEP`, act.\r\n");
    auto [item, snapshot] = deep(s.files);
    item.rules = Rules::hand_written("## Deep Mode\r\nWhen the active model role is `DEEP`, act.\nTyped line.\n");
    REQUIRE(run(s.files.save(item, "DEEP", snapshot, ACCEPT)));
    CHECK(slurp(s.files.rules) == "# Rules\r\n\r\n## Deep Mode\r\nWhen the active model role is `DEEP`, act.\r\nTyped line.\r\n");
}

TEST_CASE("a models-only edit does not create RULES.md") {
    auto s = setup("");
    fs::remove(s.files.rules);
    auto [item, snapshot] = deep(s.files);
    item.models = {Selector::model("vendor/changed")};
    REQUIRE(run(s.files.save(item, "DEEP", snapshot, ACCEPT)));
    CHECK_FALSE(fs::exists(s.files.rules));
    CHECK(contains(slurp(s.files.config), "vendor/changed"));
}

TEST_CASE("creation uses the form's models when given") {
    auto s = setup("");
    auto [_, snapshot] = must_load(s.files);
    auto item = managed("NEW");
    item.models = {Selector::model("vendor/other")};
    REQUIRE(run(s.files.save(item, std::nullopt, snapshot, ACCEPT)));
    auto [list, __] = must_load(s.files);
    CHECK(find(std::move(list), "NEW").models == std::vector{Selector::model("vendor/other")});
}

TEST_CASE("structured fields round-trip without touching neighbours") {
    for (std::string eol : {"\n", "\r\n"}) {
        const auto prefix = with_eol("# Rules\n\nKeep this.\n\n", eol);
        const auto suffix = with_eol("\n\n## Other Mode\nWhen the active model role is `OTHER`, act.\n", eol);
        // `rendered` creates the section under the role NEW (DEEP is taken); rename it to DEEP.
        const auto legacy = with_eol(replace(replace(rendered(managed("NEW")), START + "NEW", START + "DEEP"), "`NEW`", "`DEEP`"), eol);
        const auto& legacy_deep = legacy;
        auto s = setup(prefix + legacy_deep + suffix);
        auto [item, snapshot] = deep(s.files);
        CHECK(item.rules == managed("DEEP").rules);
        REQUIRE(item.rules.kind == Rules::Kind::Managed);
        item.rules.fields = ManagedFields{
            "Use for research.\n\nMore detail.",
            {{"scout", "Gather references."}},
            {"Read sources.", "Summarize findings."},
            {{"Guidance", "First paragraph.\n\n#### Detail\nKeep context.\n"}},
        };
        REQUIRE(run(s.files.save(item, "DEEP", snapshot, ACCEPT)));
        auto legacy_lf = replace(legacy_deep, "\r\n", "\n");
        REQUIRE(legacy_lf.ends_with(END));
        legacy_lf.resize(legacy_lf.size() - END.size());
        const auto expected = with_eol(legacy_lf + "\n" + FIELD + "\n### When to use\n\nUse for research.\n\nMore detail.\n\n" + FIELD +
                                           "\n### Routing\n\n- `scout`: Gather references.\n\n" + FIELD +
                                           "\n### Steps\n\n1. Read sources.\n2. Summarize findings.\n\n" + FIELD +
                                           "\n### Guidance\n\nFirst paragraph.\n\n#### Detail\nKeep context.\n\n" + END,
                                       eol);
        const auto actual = slurp(s.files.rules);
        CHECK(actual == prefix + expected + suffix);
        auto [loaded, snap] = deep(s.files);
        CHECK(loaded.rules == item.rules);
        REQUIRE(run(s.files.save(loaded, "DEEP", snap, ACCEPT)));
        CHECK(slurp(s.files.rules) == actual);
    }
}

TEST_CASE("an external body is lossless, editable and cannot be renamed") {
    for (std::string eol : {"\n", "\r\n"}) {
        const auto body =
            with_eol("Custom preamble\n\nWhen the active model role is `DEEP`, use custom instructions.\nText changed outside.  \n\n", eol);
        const auto original = "before" + eol + "  " + START + "DEEP -->" + eol + body + "  " + END + eol + "after" + eol;
        auto s = setup(original);
        auto [item, snapshot] = deep(s.files);
        CHECK(item.rules == Rules::external(body));
        REQUIRE(run(s.files.save(item, "DEEP", snapshot, ACCEPT)));
        CHECK(slurp(s.files.rules) == original);
        auto [_, snap] = deep(s.files);
        item.role = "STUDY";
        CHECK_FALSE(run(s.files.save(item, "DEEP", snap, ACCEPT)));
        item.role = "DEEP";
        const auto changed = replace(body, "outside", "inside");
        item.rules = Rules::external(changed);
        REQUIRE(run(s.files.save(item, "DEEP", snap, ACCEPT)));
        CHECK(slurp(s.files.rules) == replace(original, body, changed));
        CHECK(deep(s.files).first.rules == Rules::external(changed));
    }
}

TEST_CASE("an unsupported managed format is external and marker conflicts fail") {
    const auto canonical = rendered(managed("NEW"));
    for (const auto& altered : {
             replace(canonical, "Route work only to these agents.", "Use discretion."),
             replace(canonical, "## Research Mode", "## Changed header"),
             replace(canonical, "When the active model role is `NEW`,", "This role"),
         }) {
        auto rules = rules_of(altered, "NEW");
        REQUIRE(rules);
        CHECK(rules->kind == Rules::Kind::External);
    }
    CHECK_FALSE(rules_of(replace(canonical, "role is `NEW`", "role is `OTHER`"), "NEW"));
    CHECK_FALSE(rules_of(replace(canonical, END, "<!-- /omp-gui:workflow broken -->"), "NEW"));
}

TEST_CASE("duplicate agent routing opens as raw Markdown") {
    auto item = managed("NEW");
    const RoutingNote note{"scout", "Look up references."};
    item.rules.fields.routing = {note};
    const auto canonical = rendered(item);
    const auto edited = replace(canonical, END, "- `scout`: Read the glossary.\n" + END);
    auto expected = edited.substr(edited.find('\n') + 1);
    expected.resize(expected.size() - END.size());
    CHECK(rules_of(edited, "NEW") == Rules::external(expected));

    item.rules.fields.routing.push_back(note);
    auto s = setup("");
    auto [_, snapshot] = must_load(s.files);
    CHECK_FALSE(run(s.files.save(item, std::nullopt, snapshot, ACCEPT)));
}

TEST_CASE("notes keep whitespace and literal field markers") {
    for (const auto& notes : {std::string("  \nKeep this indentation."), ALLOWED + "this is notes\n" + FIELD + "\n" + NOTES_FIELD + "\nKeep this."}) {
        auto s = setup("");
        auto [_, snapshot] = must_load(s.files);
        auto item = managed("NEW");
        item.rules.notes = notes;
        item.rules.fields.description = "Read the glossary.";
        REQUIRE(run(s.files.save(item, std::nullopt, snapshot, ACCEPT)));
        auto [list, __] = must_load(s.files);
        CHECK(find(std::move(list), "NEW").rules == item.rules);
    }
}

TEST_CASE("reserved field markers and headings are rejected") {
    auto s = setup("");
    auto [_, snapshot] = must_load(s.files);
    auto item = managed("NEW");
    item.rules.fields.blocks.push_back({"Routing", "Text"});
    CHECK_FALSE(run(s.files.save(item, std::nullopt, snapshot, ACCEPT)));
    item.rules.fields.blocks[0] = {"Notes", "Before\n" + FIELD + "\nAfter"};
    CHECK_FALSE(run(s.files.save(item, std::nullopt, snapshot, ACCEPT)));
    CHECK(slurp(s.files.rules).empty());
}

TEST_CASE("workflow goldens: unedited saves keep bytes, edits match Rust") {
    const auto golden = fs::path(LOA_GOLDEN_DIR) / "settings" / "workflows";
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    auto files = WorkflowFiles::in_agent_dir(dir->path());
    const auto rules = slurp(golden / "RULES.md");
    const auto config = slurp(golden / "config.yml");
    spit(files.rules, rules);
    spit(files.config, config);

    {
        auto [list, snapshot] = must_load(files);
        const auto plan = find(list, "PLAN");
        CHECK(plan.rules.kind == Rules::Kind::Managed);
        CHECK(plan.rules.allowed == std::vector<std::string>{"scout", "explore"});
        CHECK(find(list, "EXT").rules.kind == Rules::Kind::External);
        CHECK(find(list, "DEEP").rules.kind == Rules::Kind::HandWritten);
        for (const auto* role : {"PLAN", "EXT", "DEEP"}) {
            const auto w = find(list, role);
            REQUIRE(run(files.save(w, role, snapshot, ACCEPT)));
        }
        CHECK(slurp(files.rules) == rules);
        CHECK(slurp(files.config) == config);
    }
    {
        auto [_, snapshot] = must_load(files);
        const Workflow research{"RESEARCH", {},
                                Rules::managed("Research", {"scout"}, {"bash"}, "Cite sources.",
                                               ManagedFields{"Use for research.\n\nMore detail.",
                                                             {{"scout", "Gather references."}},
                                                             {"Read sources.", "Summarize findings."},
                                                             {{"Guidance", "First.\n\n#### Detail\nKeep context.\n"}}})};
        REQUIRE(run(files.save(research, std::nullopt, snapshot, ACCEPT)));
        CHECK(slurp(files.rules) == slurp(golden / "RULES.created.md"));
        CHECK(slurp(files.config) == slurp(golden / "config.created.yml"));
    }
    {
        auto [list, snapshot] = must_load(files);
        auto deep_wf = find(std::move(list), "DEEP");
        deep_wf.rules = Rules::hand_written(replace(deep_wf.rules.text, "dig deep", "dig deeper"));
        deep_wf.role = "STUDY";
        REQUIRE(run(files.save(deep_wf, "DEEP", snapshot, ACCEPT)));
    }
    {
        auto [list, snapshot] = must_load(files);
        auto ext = find(std::move(list), "EXT");
        ext.rules = Rules::external("Edited body for `EXT`.\n");
        ext.models = {Selector::model("vendor/other")};
        REQUIRE(run(files.save(ext, "EXT", snapshot, ACCEPT)));
    }
    {
        auto [_, snapshot] = must_load(files);
        REQUIRE(run(files.remove("PLAN", snapshot, ACCEPT)));
    }
    CHECK(slurp(files.rules) == slurp(golden / "RULES.edited.md"));
    CHECK(slurp(files.config) == slurp(golden / "config.edited.yml"));
}
