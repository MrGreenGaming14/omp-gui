// Port of `crates/loa-ui-settings/src/agents/tests.rs`.

#include <doctest/doctest.h>

#include <QApplication>
#include <QLineEdit>

#include "loa/ui/kit/testkit.hpp"
#include "loa/ui/settings/agents/view.hpp"

// Tests live in a named namespace so the `local` helper hides the screen's `local` message namespace.
namespace agents_tests {

using namespace loa::ui::settings::agents;
namespace in = loa::view::settings::agents::intent;
using Strings = std::vector<std::string>;

void app() {
    if (QApplication::instance()) return;
    static int argc = 1;
    static char name[] = "loa_ui_settings_tests";
    static char* argv[] = {name, nullptr};
    // Leaked on purpose: a static QApplication is destroyed after Qt's input devices at exit and crashes.
    new QApplication(argc, argv);
}

AgentEntry file(const std::string& name, AgentSource source) {
    AgentFields fields{name, name + " agent", ModelSpec{std::string("anthropic/claude-sonnet")}, std::string("high"),
                       Strings{"read", "grep"}, Strings{"worker"}, "You review code.\n"};
    return AgentEntry{name + ".md", source, true, fields};
}

Choices choices(const Strings& tools) {
    return Choices{{"anthropic/claude-sonnet", "@slow"}, {"off", "low", "high"}, tools, std::nullopt};
}

State state() {
    State s;
    s.set_agents({file("reviewer", AgentSource::User), file("worker", AgentSource::Project)});
    s.set_choices(choices({"read", "grep", "edit", "bash"}));
    return s;
}

std::optional<Intent> local(State& s, Local m) { return s.update(Message{std::move(m)}); }

template <class F> void type_into(State& s, F make, const std::string& text) { CHECK_FALSE(local(s, make(text)).has_value()); }

auto name_msg = [](const std::string& t) { return Local{local::Name{t}}; };
auto description_msg = [](const std::string& t) { return Local{local::Description{t}}; };


TEST_CASE("agents: templates are blank and one copy per agent") {
    Strings titles;
    for (const auto& t : state().templates()) titles.push_back(t.label());
    CHECK(titles == Strings{"Blank", "Copy of reviewer", "Copy of worker"});
}

TEST_CASE("agents: copy template fills every field and suffixes the name") {
    auto s = state();
    auto t = s.templates().at(1);
    CHECK_FALSE(local(s, local::UseTemplate{t}).has_value());
    const auto f = s.fields();
    CHECK(f.name == "reviewer-copy");
    CHECK(f.description == "reviewer agent");
    CHECK(f.model == std::optional(ModelSpec{std::string("anthropic/claude-sonnet")}));
    CHECK(f.thinking_level == std::optional<std::string>("high"));
    CHECK(f.tools == std::optional(Strings{"read", "grep"}));
    CHECK(f.spawns == std::optional(Strings{"worker"}));
    CHECK(f.body == "You review code.\n");
    // A copy keeps the copied file's other keys but writes a new file.
    CHECK(s.draft().base == std::optional<std::filesystem::path>("reviewer.md"));
    CHECK_FALSE(s.draft().editing.has_value());
}

TEST_CASE("agents: blank template clears the form") {
    auto s = state();
    s.copy(0);
    local(s, local::UseTemplate{s.templates().at(0)});
    const auto f = s.fields();
    CHECK(f.name.empty());
    CHECK(f.description.empty());
    CHECK_FALSE(f.model.has_value());
    CHECK_FALSE(f.tools.has_value());
    CHECK_FALSE(f.spawns.has_value());
    CHECK_FALSE(s.draft().base.has_value());
}

TEST_CASE("agents: tool checkboxes change the definition's tools") {
    auto s = state();
    s.new_agent();
    CHECK_FALSE(s.fields().tools.has_value());
    local(s, local::Tool{"edit", true});
    local(s, local::Tool{"read", true});
    // Listed order, not click order.
    CHECK(s.fields().tools == std::optional(Strings{"read", "edit"}));
    local(s, local::Tool{"read", false});
    CHECK(s.fields().tools == std::optional(Strings{"edit"}));
    local(s, local::Tool{"edit", false});
    CHECK_FALSE(s.fields().tools.has_value());
}

TEST_CASE("agents: spawn checkboxes cover the other agents only") {
    auto s = state();
    s.copy(0);
    type_into(s, name_msg, "reviewer");
    CHECK(s.other_agents() == Strings{"worker"});
    local(s, local::Spawn{"worker", true});
    CHECK(s.fields().spawns == std::optional(Strings{"worker"}));
}

TEST_CASE("agents: model and thinking pickers set and clear their keys") {
    auto s = state();
    s.new_agent();
    local(s, local::Model{"@slow"});
    local(s, local::Thinking{"low"});
    auto f = s.fields();
    CHECK(f.model == std::optional(ModelSpec{std::string("@slow")}));
    CHECK(f.thinking_level == std::optional<std::string>("low"));
    local(s, local::Model{std::string(MODEL_DEFAULT)});
    local(s, local::Thinking{std::string(THINKING_DEFAULT)});
    f = s.fields();
    CHECK_FALSE(f.model.has_value());
    CHECK_FALSE(f.thinking_level.has_value());
}

TEST_CASE("agents: save asks the app to check and edits follow its errors") {
    auto s = state();
    s.new_agent();
    type_into(s, description_msg, "Does things");
    auto saved = local(s, local::Save{});
    REQUIRE(saved);
    REQUIRE(std::holds_alternative<in::Save>(*saved));
    CHECK(std::get<in::Save>(*saved).draft.fields.description == "Does things");
    const FieldError error{"name", "name is required"};
    s.set_errors({error});
    CHECK(s.errors() == std::vector{error});
    // The errors follow the edits once shown.
    auto checked = local(s, local::Name{"does-things"});
    REQUIRE(checked);
    REQUIRE(std::holds_alternative<in::Check>(*checked));
    CHECK(std::get<in::Check>(*checked).draft.fields.name == "does-things");
    s.set_errors({});
    CHECK(s.errors().empty());
    // A fresh form stops checking.
    s.new_agent();
    CHECK_FALSE(local(s, local::Name{"x"}).has_value());
}

TEST_CASE("agents: save emits the draft") {
    auto s = state();
    s.new_agent();
    type_into(s, name_msg, "does-things");
    type_into(s, description_msg, "Does things");
    local(s, local::Tool{"read", true});
    local(s, local::Tool{"bash", true});
    auto saved = local(s, local::Save{});
    REQUIRE(saved);
    REQUIRE(std::holds_alternative<in::Save>(*saved));
    const auto& draft = std::get<in::Save>(*saved).draft;
    CHECK(draft.fields.name == "does-things");
    CHECK(draft.fields.tools == std::optional(Strings{"read", "bash"}));
    CHECK_FALSE(draft.base.has_value());
    CHECK_FALSE(draft.editing.has_value());
}

TEST_CASE("agents: close emits close") {
    State a;
    CHECK(a.update(Message{Intent{in::Close{}}}) == std::optional<Intent>(in::Close{}));
    State b;
    CHECK_FALSE(b.update(Message{Intent{in::Toggle{"x.md"}}}).has_value());
}

TEST_CASE("agents: view builds with and without errors") {
    app();
    auto s = state();
    AgentsView titled;
    titled.render(s);
    s.new_agent();
    local(s, local::Save{});
    s.set_errors({FieldError{"name", "name is required"}});
    s.set_error("form", "cannot write");
    titled.render(s);
    AgentsView body(false);
    body.render(s);
    CHECK(titled.findChild<QLineEdit*>(QStringLiteral("agent-name")) != nullptr);

    // Close goes out through the signal, guarded by the state.
    std::vector<Message> sent;
    QObject::connect(&titled, &AgentsView::message, [&](const Message& m) { sent.push_back(m); });
    CHECK(loa::ui::kit::testkit::click_label(titled, "Close"));
    REQUIRE(sent.size() == 1);
    CHECK(s.update(sent.front()) == std::optional<Intent>(in::Close{}));
}

TEST_CASE("agents: replace and delete ask before their event") {
    auto s = state();
    s.edit(0);
    CHECK(s.editing() == std::optional<std::filesystem::path>("reviewer.md"));

    CHECK_FALSE(local(s, local::Delete{}).has_value());
    CHECK(s.pending_question() == std::optional<std::string>("Delete reviewer?"));
    CHECK_FALSE(local(s, local::Cancel{}).has_value());
    CHECK_FALSE(s.pending_question().has_value());
    CHECK_FALSE(local(s, local::Confirm{}).has_value());

    local(s, local::Delete{});
    auto deleted = local(s, local::Confirm{});
    CHECK(deleted == std::optional<Intent>(in::Delete{"reviewer.md"}));

    s.ask_replace(s.draft(), "reviewer.md");
    CHECK(s.pending_question() == std::optional<std::string>("Replace reviewer?"));
    auto replaced = local(s, local::Confirm{});
    REQUIRE(replaced);
    REQUIRE(std::holds_alternative<in::Replace>(*replaced));
    CHECK(std::get<in::Replace>(*replaced).draft.fields.name == "reviewer");
    auto toggled = local(s, local::Toggle{});
    REQUIRE(toggled);
    CHECK(std::holds_alternative<in::Toggle>(*toggled));
}

TEST_CASE("agents: picker menus open one at a time and close on a pick") {
    auto s = state();
    local(s, local::TogglePicker{PickerKind::Model});
    CHECK(s.open_picker() == std::optional(PickerKind::Model));
    local(s, local::TogglePicker{PickerKind::Thinking});
    CHECK(s.open_picker() == std::optional(PickerKind::Thinking));
    local(s, local::Thinking{"low"});
    CHECK_FALSE(s.open_picker().has_value());
}

} // namespace agents_tests
