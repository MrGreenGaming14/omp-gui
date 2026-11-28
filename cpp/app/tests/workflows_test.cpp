// The workflows area (port of `loa-app/src/areas/workflows/tests.rs`): reading and editing
// omp's workflows through the screen, stale-write protection, the commands and the session
// config it contributes. The role switch tests that need the models, sessions and decisions
// areas belong to those areas.

#include <doctest/doctest.h>

#include "areas/workflows/workflows.hpp"
#include "testing.hpp"

using namespace loa::app;
using loa::app::testing::read_file;
using loa::app::testing::TestApp;
using loa::app::testing::write_file;
namespace wf = loa::app::areas::workflows;
namespace screen = loa::ui::settings::workflows;
namespace shown = loa::view::settings::workflows;
namespace local = loa::ui::settings::workflows::local;
namespace fs = std::filesystem;

namespace {

struct WorkflowsApp : TestApp {
    WorkflowsApp() : TestApp({boot<wf::State>()}) {
        write_file(agent / "config.yml", "modelRoles:\n  default: provider/model\n  DEEP: provider/deep\n");
        write_file(agent / "RULES.md", "## Deep Mode\nWhen the active model role is `DEEP`.\nOriginal.\n");
        send(wf::State::message(wf::msg::Open{}));
    }

    [[nodiscard]] wf::State& area() const {
        auto* state = app->areas.get<wf::State>();
        REQUIRE(state != nullptr);
        return *state;
    }
    void local(screen::Local message) { send(wf::State::message(wf::msg::Screen{screen::Message{std::move(message)}})); }
    // Hands the screen's `intent` to the area as the screen itself emits it.
    void intent(shown::Intent intent) { send(wf::State::message(wf::msg::Screen{screen::Message{std::move(intent)}})); }
    [[nodiscard]] fs::path rules() const { return agent / "RULES.md"; }
    [[nodiscard]] shown::Workflow draft() const { return area().screen().draft().workflow; }
    // Appends `text` to the raw Markdown editor.
    void append(std::string_view text) { local(local::Text{area().screen().text() + std::string(text)}); }
};


} // namespace

TEST_CASE("workflows: loads and edits hand-written rules") {
    WorkflowsApp test;
    const auto& workflows = test.area().workflows();
    CHECK(std::ranges::any_of(workflows, [](const auto& w) { return w.rules.kind == wf::domain::Rules::Kind::HandWritten; }));
    CHECK(test.app->ctx.screen() == area_id::WORKFLOWS);
    test.local(local::Open{"DEEP"});
    test.append("Updated.\n");
    test.local(local::Save{});
    CHECK(read_file(test.rules()).find("Updated.") != std::string::npos);
    CHECK(test.notice() == "Workflow saved");
}

TEST_CASE("workflows: creates a structured workflow and reopens the same fields") {
    WorkflowsApp test;
    const auto original = read_file(test.rules());
    test.local(local::New{});
    test.local(local::Name{"Review"});
    test.local(local::Description{"Use for documentation review."});
    test.local(local::SetAccess{"reader", screen::Access::Allow});
    test.local(local::Routing{"reader", "Check examples."});
    test.local(local::AddStep{});
    test.local(local::Step{0, "Read the change."});
    test.local(local::AddStep{});
    test.local(local::Step{1, "Summarize findings."});
    test.local(local::AddBlock{});
    test.local(local::BlockHeading{0, "Reporting"});
    test.local(local::BlockText{0, "Cite the relevant files."});
    test.local(local::Save{});
    const auto written = read_file(test.rules());
    CHECK(written == original +
                         "\n<!-- omp-gui:workflow REVIEW -->\n## Review Mode\n\n"
                         "When the active model role is `REVIEW`, operate in Review Mode.\n\n"
                         "- Allowed agents: `reader`. Route work only to these agents.\n"
                         "\n<!-- loa:workflow-field -->\n### When to use\n\nUse for documentation review.\n"
                         "\n<!-- loa:workflow-field -->\n### Routing\n\n- `reader`: Check examples.\n"
                         "\n<!-- loa:workflow-field -->\n### Steps\n\n1. Read the change.\n2. Summarize findings.\n"
                         "\n<!-- loa:workflow-field -->\n### Reporting\n\nCite the relevant files.\n"
                         "<!-- /omp-gui:workflow -->\n");
    test.local(local::Open{"DEEP"});
    test.local(local::Open{"REVIEW"});
    auto draft = test.draft();
    CHECK(draft.models == std::vector<std::string>{"provider/model"});
    const shown::ManagedFields fields{
        .description = "Use for documentation review.",
        .routing = {shown::RoutingNote{"reader", "Check examples."}},
        .steps = {"Read the change.", "Summarize findings."},
        .blocks = {shown::RuleBlock{"Reporting", "Cite the relevant files."}},
    };
    CHECK(draft.rules == shown::Rules{shown::rules::Managed{"Review", {"reader"}, {}, "", fields}});
    test.local(local::CopyFrom{"REVIEW"});
    test.local(local::Name{"Second review"});
    test.local(local::Save{});
    const auto saved = read_file(test.rules());
    CHECK(saved.starts_with(written));
    CHECK(saved.find("<!-- omp-gui:workflow SECOND_REVIEW -->") != std::string::npos);
    const auto copy = test.draft();
    const auto* copied = std::get_if<shown::rules::Managed>(&copy.rules);
    REQUIRE(copied != nullptr);
    CHECK(copied->fields == fields);
}

TEST_CASE("workflows: copies hand-written rules without changing the source") {
    WorkflowsApp test;
    const auto original = read_file(test.rules());
    test.local(local::CopyFrom{"DEEP"});
    test.local(local::Name{"Study"});
    test.local(local::Save{});
    const auto written = read_file(test.rules());
    const std::string marker = "\n<!-- omp-gui:workflow STUDY -->\n";
    const auto at = written.find(marker);
    REQUIRE(at != std::string::npos);
    CHECK(written.substr(0, at) == original);
    CHECK(written.substr(at + marker.size()).find(original) != std::string::npos);
    test.local(local::Open{"STUDY"});
    const auto study = test.draft();
    const auto* managed = std::get_if<shown::rules::Managed>(&study.rules);
    REQUIRE(managed != nullptr);
    auto trimmed = original;
    while (trimmed.ends_with('\n')) trimmed.pop_back();
    CHECK(managed->notes == trimmed);
}

TEST_CASE("workflows: outside edits open as raw Markdown and save in place") {
    WorkflowsApp test;
    const std::string body =
        "## Deep Mode\n\nKeep this custom preamble.\n\nWhen the active model role is `DEEP`, read the examples.\n\n"
        "<!-- loa:workflow-field -->\n### Symbols\n\nOutside notation.\n";
    const std::string suffix = "\n## Unrelated\nKeep this too.\n";
    auto marked = [&](const std::string& b) { return "<!-- omp-gui:workflow DEEP -->\n" + b + "<!-- /omp-gui:workflow -->\n" + suffix; };
    write_file(test.rules(), marked(body));
    test.send(wf::State::message(wf::msg::Edit{"DEEP"}));
    CHECK(std::holds_alternative<shown::rules::External>(test.draft().rules));
    test.local(local::Save{});
    auto external_edit = body;
    external_edit.replace(external_edit.find("read the examples"), std::string_view("read the examples").size(), "review the examples");
    test.intent(shown::intent::Close{});
    CHECK_FALSE(test.app->ctx.screen().has_value());
    write_file(test.rules(), marked(external_edit));
    test.send(wf::State::message(wf::msg::Open{}));
    test.local(local::Save{});
    CHECK(read_file(test.rules()) == marked(external_edit));
    test.append("Add a source link.\n");
    test.local(local::Save{});
    const auto saved = read_file(test.rules());
    const auto edited = external_edit + "Add a source link.\n";
    CHECK(saved == marked(edited));
    test.local(local::CopyFrom{"DEEP"});
    test.local(local::Save{});
    const auto copied = read_file(test.rules());
    CHECK(copied.starts_with(saved));
    CHECK(copied.find("<!-- omp-gui:workflow DEEP_COPY -->") != std::string::npos);
    const auto copy = test.draft();
    const auto* managed = std::get_if<shown::rules::Managed>(&copy.rules);
    REQUIRE(managed != nullptr);
    auto trimmed = edited;
    while (trimmed.ends_with('\n')) trimmed.pop_back();
    CHECK(managed->notes == trimmed);
}

TEST_CASE("workflows: saving disables editor actions until the file is reloaded") {
    WorkflowsApp test;
    test.local(local::New{});
    test.local(local::Name{"Review"});
    test.local(local::Description{"Original draft."});
    auto save = test.app->update(wf::State::message(wf::msg::Screen{screen::Message{screen::Local{local::Save{}}}}));
    CHECK(test.area().busy() == std::optional<std::string_view>("Saving workflow…"));
    test.local(local::Description{"Late edit."});
    const auto draft = test.draft();
    const auto* managed = std::get_if<shown::rules::Managed>(&draft.rules);
    REQUIRE(managed != nullptr);
    CHECK(managed->fields.description == "Original draft.");
    test.run(std::move(save));
    CHECK_FALSE(test.area().busy().has_value());
    test.local(local::Description{"Next edit."});
    test.local(local::Save{});
    CHECK(read_file(test.rules()).find("Next edit.") != std::string::npos);
}

TEST_CASE("workflows: a stale save does not overwrite external changes") {
    WorkflowsApp test;
    test.local(local::Open{"DEEP"});
    write_file(test.rules(), "outside edit\n");
    test.local(local::Save{});
    CHECK(read_file(test.rules()) == "outside edit\n");
    CHECK(test.area().screen().error() == std::optional<shown::Error>(shown::error::ChangedOnDisk{}));
    CHECK(test.area().workflows().size() == 2);
}

TEST_CASE("workflows: reload then save keeps the version on disk") {
    WorkflowsApp test;
    test.local(local::Open{"DEEP"});
    test.append("Draft.\n");
    const std::string outside = "## Deep Mode\nWhen the active model role is `DEEP`.\nChanged outside.\n";
    write_file(test.rules(), outside);
    test.local(local::Save{});
    CHECK(read_file(test.rules()) == outside);
    test.intent(shown::intent::Reload{});
    test.local(local::Save{});
    CHECK(read_file(test.rules()) == outside);
}

TEST_CASE("workflows: a harness other than omp cannot pick a workflow") {
    WorkflowsApp test;
    test.send(wf::State::message(wf::msg::Pick{"DEEP"}));
    CHECK(test.notice() == "Available with OMP sessions");
}

TEST_CASE("workflows: the selected role is disclosed in an omp session's config") {
    WorkflowsApp test;
    const auto& area = test.area();
    loa::contracts::SessionConfig config;
    config.model_role = "DEEP";
    area.contribute(config, Target{"omp", nullptr, false}, test.app->ctx);
    CHECK(config.instructions == std::optional<std::string>("The active model role is `DEEP`. Follow the RULES.md section for that role."));
    loa::contracts::SessionConfig other;
    other.model_role = "DEEP";
    area.contribute(other, Target{"claude", nullptr, false}, test.app->ctx);
    CHECK_FALSE(other.instructions.has_value());
    loa::contracts::SessionConfig none;
    area.contribute(none, Target{"omp", nullptr, true}, test.app->ctx);
    CHECK_FALSE(none.instructions.has_value());
}

TEST_CASE("workflows: commands edit and use every workflow") {
    WorkflowsApp test;
    test.intent(shown::intent::Close{});
    std::vector<std::string> titles;
    for (const auto& row : test.area().commands(test.app->ctx)) titles.push_back(row.title);
    for (const auto* title : {"New workflow", "Edit workflow: DEEP", "Edit workflow: default", "Use workflow: DEEP"})
        CHECK(std::ranges::find(titles, title) != titles.end());
    test.command("workflows.edit.DEEP");
    CHECK(test.app->ctx.screen() == area_id::WORKFLOWS);
    CHECK(test.area().screen().selected() == std::optional<std::string_view>("DEEP"));
    test.command("workflows.edit.MISSING");
    CHECK(test.notice() == "No workflow MISSING");
    CHECK_FALSE(test.area().screen().selected().has_value());
    test.command("workflows.new");
    CHECK_FALSE(test.area().screen().selected().has_value());
}

TEST_CASE("workflows: another models page is forwarded to the models area") {
    WorkflowsApp test;
    auto task = test.app->update(wf::State::message(wf::msg::ShowPage{wf::Page::Settings}));
    // Without a models area the forward ends in its refusal; the workflows page itself stays.
    CHECK(test.app->ctx.screen() == area_id::WORKFLOWS);
    test.run(std::move(task));
    test.send(wf::State::message(wf::msg::ShowPage{wf::Page::Workflows}));
    CHECK(test.app->ctx.screen() == area_id::WORKFLOWS);
}
