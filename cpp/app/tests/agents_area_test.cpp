// The agents area driven through `App::update` (port of `loa-app/src/areas/agents/{tests,live}.rs`):
// omp's directory and models come from the test app's offline model backend and the tools are
// fixed, so nothing here starts omp except the opt-in live case (`LOA_LIVE_OMP=1`).

#include <algorithm>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <optional>
#include <sstream>
#include <string>
#include <utility>
#include <vector>

#include <QApplication>
#include <QJsonDocument>
#include <QJsonObject>
#include <QProcess>
#include <QProcessEnvironment>

#include <doctest/doctest.h>

#include "areas/agents/agents.hpp"
#include "loa/settings/models/agents.hpp"
#include "support/test_app.hpp"

namespace {

namespace fs = std::filesystem;
namespace am = loa::settings::models::agents;
namespace local = loa::ui::settings::agents::local;
using loa::app::areas::agents::OMP_ONLY;
using loa::app::areas::agents::State;
using loa::app::testing::FakeHarness;
using loa::app::testing::TestApp;
namespace msg = loa::app::areas::agents::msg;
using Local = loa::ui::settings::agents::Local;

const std::vector<std::string> TOOLS{"read", "edit", "grep", "bash"};

// An app whose only harness is `harness`, with omp's agent directory at `<home>/omp`.
std::pair<std::unique_ptr<TestApp>, fs::path> omp_app(const std::string& harness) {
    auto test = std::make_unique<TestApp>(FakeHarness(harness));
    test->area<State>().use_tools(TOOLS);
    auto agent_dir = test->dir.path() / "omp";
    return {std::move(test), agent_dir};
}

void open(TestApp& test) { test.send(State::message(loa::app::to::agents::Open{})); }

void fill(TestApp& test, Local message) {
    test.send(State::message(msg::Screen{loa::ui::settings::agents::Message{std::move(message)}}));
}

const loa::ui::settings::agents::State& form(const TestApp& test) { return test.area<State>().form(); }

// Whether the palette lists a command titled `title`.
bool listed(TestApp& test, const std::string& title) {
    auto titles = test.palette_titles();
    return std::ranges::find(titles, title) != titles.end();
}

bool has_error(const TestApp& test, std::string_view field) {
    return std::ranges::any_of(form(test).errors(), [&](const auto& error) { return error.field == field; });
}

// Saves a new agent `name` with `description` through the form.
void save_new(TestApp& test, const std::string& name, const std::string& description) {
    open(test);
    fill(test, local::Name{name});
    fill(test, local::Description{description});
    fill(test, local::Save{});
}

std::optional<std::string> description_on_disk(const fs::path& agent_dir, std::string_view name) {
    auto list = am::list(agent_dir, std::nullopt);
    for (const auto& file : list.files)
        if (file.definition.name() == name) return std::string(file.definition.description());
    return std::nullopt;
}

void write_file(const fs::path& path, std::string_view text) {
    fs::create_directories(path.parent_path());
    std::ofstream(path, std::ios::binary) << text;
}

std::string read_file(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::stringstream out;
    out << in.rdbuf();
    return out.str();
}

std::optional<std::string_view> screen(const TestApp& test) { return test.app->ctx.screen(); }

// The widgets the area draws need a QApplication; leaked, as one destroyed at exit crashes.
void ensure_qapp() {
    static int argc = 1;
    static char name[] = "loa_app_tests";
    static char* argv[] = {name, nullptr};
    if (!qApp) (void)new QApplication(argc, argv);
}

} // namespace

TEST_CASE("the form opens with omp") {
    ensure_qapp();
    auto [test, _] = omp_app("omp");
    open(*test);
    CHECK(screen(*test) == State::ID);
    CHECK(form(*test).errors().empty());
    auto* body = test->area<State>().configs_body(test->app->ctx, [](loa::app::Message) {});
    CHECK_MESSAGE(body != nullptr, "Configs › Agents draws the form");
    delete body;
}

TEST_CASE("other harnesses cannot open the agents form") {
    auto [test, _] = omp_app("fake-a");
    open(*test);
    CHECK(screen(*test) == std::nullopt);
    CHECK_MESSAGE(test->area<State>().configs_body(test->app->ctx, [](loa::app::Message) {}) == nullptr,
                  "Configs › Agents keeps the file list");

    CHECK_FALSE(listed(*test, "New agent"));
    test->command("agents.new");
    CHECK(screen(*test) == std::nullopt);
    CHECK(test->notice() == OMP_ONLY);
}

TEST_CASE("agent palette commands are listed with omp") {
    auto [test, _] = omp_app("omp");
    CHECK(listed(*test, "New agent"));
    test->command("agents.new");
    CHECK(screen(*test) == State::ID);
}

TEST_CASE("save writes the agent with exactly the ticked tools") {
    auto [test, agent_dir] = omp_app("omp");
    open(*test);
    fill(*test, local::Name{"reviewer"});
    fill(*test, local::Description{"Reviews diffs"});
    fill(*test, local::Tool{"grep", true});
    fill(*test, local::Tool{"read", true});
    fill(*test, local::Save{});

    auto path = agent_dir / "agents" / "reviewer.md";
    CHECK(fs::is_regular_file(path));
    auto list = am::list(agent_dir, std::nullopt);
    CHECK(list.broken.empty());
    REQUIRE(list.files.size() == 1);
    CHECK(list.files[0].definition.name() == "reviewer");
    CHECK(list.files[0].definition.description() == "Reviews diffs");
    CHECK(list.files[0].definition.tools() == std::optional<std::vector<std::string>>{{"read", "grep"}});
    CHECK(test->notice().starts_with("Saved reviewer. omp sessions started from now on can use it"));
    CHECK(test->notice().find("already running") != std::string::npos);

    // The list refreshed: the palette offers the new agent.
    CHECK(listed(*test, "Edit agent: reviewer"));
    test->app->ctx.close_screen();
    test->command("agents.edit.reviewer");
    CHECK(screen(*test) == State::ID);
    CHECK(form(*test).name() == "reviewer");
}

TEST_CASE("invalid agent forms show their errors and write nothing") {
    auto [test, agent_dir] = omp_app("omp");
    open(*test);
    fill(*test, local::Description{"No name"});
    fill(*test, local::Save{});
    CHECK(has_error(*test, "name"));
    CHECK_FALSE(fs::exists(agent_dir / "agents"));
}

TEST_CASE("save with an empty agent name shows the error and writes nothing") {
    auto [test, agent_dir] = omp_app("omp");
    open(*test);
    fill(*test, local::Description{"Does things"});
    fill(*test, local::Save{});
    CHECK(has_error(*test, "name"));
    CHECK_FALSE(has_error(*test, "description"));
    // The errors follow the edits once shown.
    fill(*test, local::Name{"does-things"});
    CHECK(form(*test).errors().empty());
    CHECK_FALSE(fs::exists(agent_dir / "agents"));
}

TEST_CASE("an unknown tool of a copied agent is reported") {
    auto [test, agent_dir] = omp_app("omp");
    test->area<State>().use_tools({"read"});
    write_file(agent_dir / "agents" / "reviewer.md", "---\nname: reviewer\ndescription: Reviews\ntools:\n  - read\n  - grep\n---\n");
    open(*test);
    auto templates = form(*test).templates();
    REQUIRE(templates.size() > 1);
    fill(*test, local::UseTemplate{templates[1]});
    fill(*test, local::Save{});
    CHECK(std::ranges::any_of(form(*test).errors(), [](const auto& error) {
        return error.field == "tools" && error.message.find("grep") != std::string::npos;
    }));
    CHECK_FALSE(fs::exists(agent_dir / "agents" / "reviewer-copy.md"));
}

TEST_CASE("a copy keeps the keys the form has no field for") {
    auto [test, agent_dir] = omp_app("omp");
    write_file(agent_dir / "agents" / "plan.md", "---\nname: plan\ndescription: Plans\noutput:\n  type: string\n---\nPlan.\n");
    open(*test);
    auto templates = form(*test).templates();
    REQUIRE(templates.size() > 1);
    fill(*test, local::UseTemplate{templates[1]});
    fill(*test, local::Save{});
    auto copy = read_file(agent_dir / "agents" / "plan-copy.md");
    CHECK_MESSAGE((copy.find("name: plan-copy") != std::string::npos && copy.find("output:\n  type: string\n") != std::string::npos),
                  copy);
}

TEST_CASE("saving an existing agent name replaces the file only after confirming") {
    auto [test, agent_dir] = omp_app("omp");
    save_new(*test, "reviewer", "First");
    test->app->ctx.close_screen();
    save_new(*test, "reviewer", "Second");
    // Asked, nothing written yet.
    CHECK(description_on_disk(agent_dir, "reviewer") == "First");
    CHECK(form(*test).pending_question() == "Replace reviewer?");

    fill(*test, local::Cancel{});
    CHECK(description_on_disk(agent_dir, "reviewer") == "First");
    CHECK(form(*test).pending_question() == std::nullopt);

    fill(*test, local::Save{});
    fill(*test, local::Confirm{});
    CHECK(description_on_disk(agent_dir, "reviewer") == "Second");
    CHECK(test->notice().starts_with("Saved reviewer."));
}

TEST_CASE("replacing a disabled agent keeps it disabled") {
    auto [test, agent_dir] = omp_app("omp");
    save_new(*test, "reviewer", "First");
    fill(*test, local::Toggle{});
    CHECK(fs::is_regular_file(agent_dir / "agents" / "reviewer.md.disabled"));
    test->app->ctx.close_screen();
    save_new(*test, "reviewer", "Second");
    fill(*test, local::Confirm{});
    CHECK_FALSE(fs::exists(agent_dir / "agents" / "reviewer.md"));
    CHECK(fs::is_regular_file(agent_dir / "agents" / "reviewer.md.disabled"));
    CHECK(description_on_disk(agent_dir, "reviewer") == "Second");
}

TEST_CASE("editing an agent saves back to its own file without asking") {
    auto [test, agent_dir] = omp_app("omp");
    save_new(*test, "reviewer", "First");
    fill(*test, local::Description{"Edited"});
    fill(*test, local::Save{});
    CHECK(form(*test).pending_question() == std::nullopt);
    CHECK(description_on_disk(agent_dir, "reviewer") == "Edited");
}

TEST_CASE("disabling and enabling an agent renames the file and marks the list") {
    auto [test, agent_dir] = omp_app("omp");
    save_new(*test, "reviewer", "First");
    auto enabled = agent_dir / "agents" / "reviewer.md";
    auto disabled = agent_dir / "agents" / "reviewer.md.disabled";

    fill(*test, local::Toggle{});
    CHECK((!fs::exists(enabled) && fs::is_regular_file(disabled)));
    auto list = am::list(agent_dir, std::nullopt);
    REQUIRE_MESSAGE(list.files.size() == 1, "a disabled agent stays listed");
    CHECK_FALSE(list.files[0].enabled);
    CHECK(form(*test).is_enabled(disabled) == false);
    CHECK(test->notice().starts_with("Disabled reviewer."));
    CHECK(listed(*test, "Edit agent: reviewer"));

    fill(*test, local::Toggle{});
    CHECK((fs::is_regular_file(enabled) && !fs::exists(disabled)));
    CHECK(form(*test).is_enabled(enabled) == true);
    CHECK(test->notice().starts_with("Enabled reviewer."));
}

TEST_CASE("deleting an agent removes the file only after confirming") {
    auto [test, agent_dir] = omp_app("omp");
    save_new(*test, "reviewer", "First");
    auto path = agent_dir / "agents" / "reviewer.md";

    fill(*test, local::Delete{});
    CHECK(fs::is_regular_file(path));
    CHECK(form(*test).pending_question() == "Delete reviewer?");
    fill(*test, local::Cancel{});
    CHECK(fs::is_regular_file(path));

    fill(*test, local::Delete{});
    fill(*test, local::Confirm{});
    CHECK_FALSE(fs::exists(path));
    CHECK(test->notice().starts_with("Deleted reviewer."));
    CHECK_FALSE(listed(*test, "Edit agent: reviewer"));
}

TEST_CASE("an agent write error keeps the form with the reason at the top") {
    auto [test, agent_dir] = omp_app("omp");
    // A file where the agents directory should be makes the write fail.
    write_file(agent_dir / "agents", "not a directory");
    save_new(*test, "reviewer", "First");
    CHECK(screen(*test) == State::ID);
    CHECK(has_error(*test, "form"));
    CHECK(form(*test).name() == "reviewer");
}

TEST_CASE("agent definitions round-trip byte-identically through the form") {
    auto [test, agent_dir] = omp_app("omp");
    // Written by the form: what `agents` reads back renders to the same bytes.
    save_new(*test, "reviewer", "Reviews diffs");
    auto saved_path = agent_dir / "agents" / "reviewer.md";
    auto saved = read_file(saved_path);
    REQUIRE_FALSE(saved.empty());
    auto parsed = am::parse(saved);
    REQUIRE(parsed.has_value());
    CHECK(parsed->to_text() == saved);
    // Saved again unchanged from the form: the file keeps its bytes.
    fill(*test, local::Save{});
    CHECK(read_file(saved_path) == saved);

    // A hand-written file with comments, quoting and keys the form has no field for, saved
    // through the form without edits, keeps its bytes.
    const std::string hand = "---\n# Planner\nname: plan\ndescription: \"Plans: carefully\"\nmodel: \"@slow\"\noutput:\n  type: string\n"
                             "tools: [read, grep]\n---\nPlan it.\r\n\nThen stop.\n";
    auto hand_path = agent_dir / "agents" / "plan.md";
    write_file(hand_path, hand);
    test->app->ctx.close_screen();
    test->command("agents.edit.plan");
    REQUIRE(form(*test).name() == "plan");
    fill(*test, local::Save{});
    CHECK(form(*test).errors().empty());
    CHECK(read_file(hand_path) == hand);
    auto reread = am::parse(read_file(hand_path));
    REQUIRE(reread.has_value());
    CHECK(reread->to_text() == hand);
}

namespace {

void write_models(const fs::path& agent_dir) {
    write_file(agent_dir / "models.yml", R"({
  "providers": {
    "loa-test": {
      "baseUrl": "http://127.0.0.1:9/v1",
      "apiKey": "LOA_TEST_UNUSED_KEY",
      "api": "openai-completions",
      "models": [
        {
          "id": "never-called",
          "name": "never called",
          "reasoning": false,
          "input": ["text"],
          "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
          "contextWindow": 8192,
          "maxTokens": 1024
        }
      ]
    }
  }
}
)");
}

// Starts omp in RPC mode over `agent_dir` and returns the text of its `get_state` response.
std::string omp_state(const fs::path& agent_dir, const fs::path& cwd) {
    QProcess omp;
    auto env = QProcessEnvironment::systemEnvironment();
    env.insert(QStringLiteral("PI_CODING_AGENT_DIR"), QString::fromStdString(agent_dir.string()));
    env.insert(QStringLiteral("LOA_TEST_UNUSED_KEY"), QStringLiteral("unused"));
    omp.setProcessEnvironment(env);
    omp.setWorkingDirectory(QString::fromStdString(cwd.string()));
    omp.setStandardErrorFile(QProcess::nullDevice());
    omp.start(QStringLiteral("omp"), {QStringLiteral("--mode"), QStringLiteral("rpc"), QStringLiteral("--no-session"),
                                      QStringLiteral("--no-extensions"), QStringLiteral("--no-skills"), QStringLiteral("--offline"),
                                      QStringLiteral("--model"), QStringLiteral("loa-test/never-called")});
    REQUIRE_MESSAGE(omp.waitForStarted(), "omp on PATH");
    omp.write("{\"type\":\"get_state\",\"id\":\"s1\"}\n");
    std::optional<std::string> state;
    while (!state && (omp.canReadLine() || omp.waitForReadyRead(60'000))) {
        while (omp.canReadLine()) {
            auto line = omp.readLine();
            auto frame = QJsonDocument::fromJson(line).object();
            if (frame.value(QStringLiteral("id")).toString() != QStringLiteral("s1")) continue;
            CHECK_MESSAGE(frame.value(QStringLiteral("success")).toBool(), line.toStdString());
            state = line.toStdString();
            break;
        }
    }
    omp.closeWriteChannel();
    omp.kill();
    omp.waitForFinished();
    REQUIRE_MESSAGE(state.has_value(), "omp answered get_state");
    return *state;
}

} // namespace

// Runs the installed omp (needs only `omp` on PATH; no model call, no credentials). omp refuses
// to start without a model, so it runs in its own agent directory with one placeholder model
// that is never called; the user's omp sign-ins and settings are never read.
TEST_CASE("live omp lists a saved agent and not a disabled one") {
    if (const char* live = std::getenv("LOA_LIVE_OMP"); !live || std::string_view(live) != "1") {
        MESSAGE("skipped: set LOA_LIVE_OMP=1 to run the installed omp");
        return;
    }
    auto [test, agent_dir] = omp_app("omp");
    fs::create_directories(agent_dir);
    write_models(agent_dir);

    open(*test);
    fill(*test, local::Name{"loa-live-enabled"});
    fill(*test, local::Description{"Saved by the app"});
    fill(*test, local::Tool{"read", true});
    fill(*test, local::Save{});
    test->app->ctx.close_screen();
    save_new(*test, "loa-live-disabled", "Saved then disabled");
    fill(*test, local::Toggle{});

    auto list = am::list(agent_dir, std::nullopt);
    CHECK(list.broken.empty());
    auto enabled = std::ranges::find_if(list.files, [](const am::AgentFile& file) { return file.definition.name() == "loa-live-enabled"; });
    REQUIRE(enabled != list.files.end());
    CHECK(enabled->definition.tools() == std::optional<std::vector<std::string>>{{"read"}});
    CHECK(std::ranges::any_of(list.files, [](const am::AgentFile& file) {
        return file.definition.name() == "loa-live-disabled" && !file.enabled;
    }));

    auto state = omp_state(agent_dir, test->dir.path());
    CHECK_MESSAGE(state.find("loa-live-enabled") != std::string::npos, "omp lists the saved agent");
    CHECK_MESSAGE(state.find("loa-live-disabled") == std::string::npos, "omp ignores the disabled agent");
}
