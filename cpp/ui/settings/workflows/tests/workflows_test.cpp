// Port of `crates/loa-ui-settings/src/workflows/tests.rs`. Text editor pastes become the editor's
// whole text after the edit, as the Qt view reports them.

#include <doctest/doctest.h>

#include <limits>

#include <QAbstractButton>
#include <QApplication>
#include <QLabel>
#include <QLineEdit>

#include "loa/ui/settings/workflows/view.hpp"

// Tests live in a named namespace so the `local` helper hides the screen's `local` message namespace.
namespace workflows_tests {

using namespace loa::ui::settings::workflows;
namespace in = loa::view::settings::workflows::intent;
namespace rules = loa::view::settings::workflows::rules;
using Strings = std::vector<std::string>;
constexpr auto MAX = std::numeric_limits<std::size_t>::max();

void app() {
    if (QApplication::instance()) return;
    static int argc = 1;
    static char name[] = "loa_ui_settings_tests";
    static char* argv[] = {name, nullptr};
    // Leaked on purpose: a static QApplication is destroyed after Qt's input devices at exit and crashes.
    new QApplication(argc, argv);
}

State state() {
    State s;
    s.set_workflows({
        Workflow{"RESEARCH", {"provider/model:high"}, rules::Managed{"Research", {"scout"}, {"builder"}, "Be brief.", ManagedFields{}}},
        Workflow{"DEEP", {}, rules::HandWritten{"## Deep Mode\nOriginal\n"}},
        Workflow{"EMPTY", {}, rules::None{}},
    });
    s.set_agents({AgentInfo{"scout", true}, AgentInfo{"builder", true}, AgentInfo{"critic", false}});
    return s;
}

std::optional<Intent> local(State& s, Local m) { return s.update(Message{std::move(m)}); }

rules::Managed managed(const State& s) {
    const auto rules = s.draft().workflow.rules;
    REQUIRE(std::holds_alternative<rules::Managed>(rules));
    return std::get<rules::Managed>(rules);
}

// Every text a label or button under `root` shows, and every accessible name.
bool shows(QWidget& root, std::string_view text) {
    const auto t = QString::fromUtf8(text.data(), static_cast<qsizetype>(text.size()));
    for (auto* l : root.findChildren<QLabel*>())
        if (l->text() == t) return true;
    for (auto* b : root.findChildren<QAbstractButton*>())
        if (b->text() == t || b->accessibleName() == t) return true;
    for (auto* w : root.findChildren<QWidget*>())
        if (w->accessibleName() == t) return true;
    return false;
}


TEST_CASE("workflows: edit managed and save") {
    auto s = state();
    REQUIRE(s.edit("RESEARCH"));
    CHECK(s.access("scout") == Access::Allow);
    CHECK(s.access("builder") == Access::Disallow);
    local(s, local::SetAccess{"scout", Access::Unspecified});
    local(s, local::SetAccess{"critic", Access::Allow});
    local(s, local::Name{"Review"});
    auto saved = local(s, local::Save{});
    REQUIRE(saved);
    REQUIRE(std::holds_alternative<in::Save>(*saved));
    const auto& save = std::get<in::Save>(*saved);
    CHECK(save.previous_role == std::optional<std::string>("RESEARCH"));
    REQUIRE(std::holds_alternative<rules::Managed>(save.draft.workflow.rules));
    const auto& m = std::get<rules::Managed>(save.draft.workflow.rules);
    CHECK(m.name == "Review");
    CHECK(m.allowed == Strings{"critic"});
    CHECK(m.disallowed == Strings{"builder"});
    CHECK(save.draft.workflow.models == Strings{"provider/model:high"});
}

TEST_CASE("workflows: hand-written edit is saved as raw text") {
    auto s = state();
    REQUIRE(s.edit("DEEP"));
    local(s, local::Text{"New " + s.text()});
    auto saved = local(s, local::Save{});
    REQUIRE(saved);
    const auto& save = std::get<in::Save>(*saved);
    CHECK(save.previous_role == std::optional<std::string>("DEEP"));
    REQUIRE(std::holds_alternative<rules::HandWritten>(save.draft.workflow.rules));
    const auto& text = std::get<rules::HandWritten>(save.draft.workflow.rules).text;
    CHECK(text.find("New ") != std::string::npos);
    CHECK(text.find("## Deep Mode") != std::string::npos);
}

TEST_CASE("workflows: delete requires confirmation and can be cancelled") {
    auto s = state();
    s.edit("DEEP");
    CHECK_FALSE(local(s, local::ConfirmDelete{}).has_value());
    local(s, local::Delete{});
    local(s, local::CancelDelete{});
    CHECK_FALSE(local(s, local::ConfirmDelete{}).has_value());
    local(s, local::Delete{});
    CHECK(local(s, local::ConfirmDelete{}) == std::optional<Intent>(in::Delete{"DEEP"}));
}

TEST_CASE("workflows: new and selection emit intents") {
    auto s = state();
    CHECK(local(s, local::Open{"RESEARCH"}) == std::optional<Intent>(in::Open{"RESEARCH"}));
    CHECK(local(s, local::New{}) == std::optional<Intent>(in::New{}));
    CHECK_FALSE(s.selected().has_value());
    CHECK(s.draft().workflow.role.empty());
}

TEST_CASE("workflows: view displays workflows, agents and reload") {
    app();
    auto s = state();
    s.edit("RESEARCH");
    s.set_error(Error{loa::view::settings::workflows::error::ChangedOnDisk{}});
    WorkflowsView view;
    view.render(s);
    for (const char* text : {"New workflow", "RESEARCH", "DEEP", "Copy from DEEP\u2026", "EMPTY", "Description", "Routing note", "Add step",
                             "Add block", "critic (disabled)", "Save", "Delete", "Reload", "Close"})
        CHECK_MESSAGE(shows(view, text), "missing ", text);
    // Reload goes out only while the file changed on disk.
    CHECK(s.update(Message{Intent{in::Reload{}}}) == std::optional<Intent>(in::Reload{}));
    s.set_error(std::nullopt);
    CHECK_FALSE(s.update(Message{Intent{in::Reload{}}}).has_value());
}

TEST_CASE("workflows: structured fields edit, reorder and bound indices") {
    auto s = state();
    s.edit("RESEARCH");
    local(s, local::Description{"A short description"});
    local(s, local::Routing{"scout", "Find context"});
    local(s, local::Routing{"builder", "Ignored"});
    local(s, local::AddStep{});
    local(s, local::Step{0, "First"});
    local(s, local::AddStep{});
    local(s, local::Step{1, "Second"});
    local(s, local::MoveStepUp{1});
    local(s, local::MoveStepDown{0});
    local(s, local::MoveStepDown{MAX});
    local(s, local::RemoveStep{MAX});
    local(s, local::MoveStepUp{1});
    local(s, local::RemoveStep{1});
    local(s, local::AddBlock{});
    local(s, local::BlockHeading{0, "Guidance"});
    local(s, local::BlockText{0, "Some **markdown**"});
    local(s, local::BlockHeading{MAX, "Ignored"});
    local(s, local::RemoveBlock{MAX});
    auto fields = managed(s).fields;
    CHECK(fields.description == "A short description");
    CHECK(fields.routing == std::vector{RoutingNote{"scout", "Find context"}});
    CHECK(fields.steps == Strings{"Second"});
    REQUIRE(fields.blocks.size() == 1);
    CHECK(fields.blocks[0].heading == "Guidance");
    CHECK(fields.blocks[0].text.find("Some **markdown**") != std::string::npos);
    local(s, local::RemoveBlock{0});
    CHECK(managed(s).fields.blocks.empty());
}

TEST_CASE("workflows: clearing a routing note or disallowing its agent removes it") {
    auto s = state();
    s.edit("RESEARCH");
    local(s, local::Routing{"scout", "Find context"});
    local(s, local::Routing{"scout", ""});
    CHECK(managed(s).fields.routing.empty());
    local(s, local::Routing{"scout", "Find context"});
    local(s, local::SetAccess{"scout", Access::Disallow});
    const auto m = managed(s);
    CHECK(m.fields.routing.empty());
    CHECK(std::ranges::find(m.disallowed, "scout") != m.disallowed.end());
}

TEST_CASE("workflows: copy hand-written preserves source and creates independent draft") {
    auto s = state();
    s.edit("RESEARCH");
    CHECK_FALSE(local(s, local::CopyFrom{"DEEP"}).has_value());
    CHECK_FALSE(s.selected().has_value());
    CHECK(s.draft().workflow.role.empty());
    const auto m = managed(s);
    CHECK(m.name == "DEEP copy");
    CHECK(m.notes == "## Deep Mode\nOriginal\n");
    CHECK(m.fields == ManagedFields{});
    auto saved = local(s, local::Save{});
    REQUIRE(saved);
    CHECK_FALSE(std::get<in::Save>(*saved).previous_role.has_value());
    REQUIRE(s.edit("DEEP"));
    CHECK(s.draft().workflow.rules == Rules{rules::HandWritten{"## Deep Mode\nOriginal\n"}});
}

TEST_CASE("workflows: external rules are raw and cannot be renamed") {
    app();
    auto s = state();
    s.set_workflows({Workflow{"EXTERNAL", {}, rules::External{"External\n"}}});
    REQUIRE(s.edit("EXTERNAL"));
    local(s, local::Role{"OTHER"});
    CHECK(s.draft().workflow.role == "EXTERNAL");
    CHECK(s.draft().workflow.rules == Rules{rules::External{"External\n"}});
    local(s, local::Text{"New " + s.text()});
    const auto rules = s.draft().workflow.rules;
    REQUIRE(std::holds_alternative<rules::External>(rules));
    const auto& text = std::get<rules::External>(rules).text;
    CHECK(text.find("New ") != std::string::npos);
    CHECK(text.find("External") != std::string::npos);
    WorkflowsView view;
    view.render(s);
    CHECK(shows(view, "Edited outside Loa \u2014 edit the raw Markdown below; this workflow cannot be renamed here."));
    CHECK(view.findChild<QLineEdit*>(QStringLiteral("workflow-role")) == nullptr);
}

} // namespace workflows_tests
