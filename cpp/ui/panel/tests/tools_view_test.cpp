// Click-to-message checks of the agents and tools views: the Rust
// `every_section_renders_and_its_controls_click` and the view half of
// `agents_and_jobs_split_by_kind_and_emit_open_or_transcript`, without pixels.

#include <doctest/doctest.h>

#include <QAbstractButton>
#include <QApplication>
#include <QLabel>

#include "loa/ui/kit/testkit.hpp"
#include "loa/ui/panel/agents_view.hpp"
#include "loa/ui/panel/tools_view.hpp"

using namespace loa::ui::panel;
namespace ti = loa::view::panel::tools_intent;
using loa::ui::kit::testkit::click_label;

namespace {

QApplication& app() {
    static int argc = 1;
    static char name[] = "loa_ui_panel_tests";
    static char* argv[] = {name, nullptr};
    static auto* a = qApp ? static_cast<QApplication*>(qApp) : new QApplication(argc, argv);
    return *a;
}

bool shows_text(QWidget& root, std::string_view text) {
    const auto want = QString::fromUtf8(text.data(), static_cast<qsizetype>(text.size()));
    for (auto* l : root.findChildren<QLabel*>())
        if (l->text() == want) return true;
    for (auto* b : root.findChildren<QAbstractButton*>())
        if (b->text() == want || b->accessibleName() == want) return true;
    return false;
}

tools::ToolInfo tool(std::string name) { return {name, name + " tool", {}}; }

tools::McpServer server(std::string name, tools::ServerKind kind, tools::ServerStatus status, std::initializer_list<const char*> names) {
    tools::McpServer s{name, loa::contracts::McpStdio{name, {}, {}}, std::move(status), kind, {}};
    for (const char* t : names) s.tools.push_back(tool(t));
    return s;
}

} // namespace

TEST_CASE("every tools section renders and its controls click") {
    app();
    tools::State s;
    s.set_servers({server("remote", tools::ServerKind::AppHosted, tools::server_status::Running{}, {"remote_bash", "remote_file"}),
                   server("browser", tools::ServerKind::Surface, tools::server_status::Stopped{}, {"surface_click", "read"}),
                   server("git", tools::ServerKind::External, tools::server_status::Error{"exited"}, {"git_log"})});
    s.set_projects({{"a", "Project a"}, {"b", "Project b"}});
    s.set_hosts({{"build", "build.lan", std::nullopt, std::nullopt, std::nullopt, {}}});
    s.select_project("a");
    s.set_prompts({{9, tools::PromptKind::Approval, "Run?", "uptime"}});
    tools::BuiltinTools builtin{"omp", {}, {}, std::nullopt};
    for (int i = 0; i < 20; ++i) builtin.harness_tools.push_back(tool(std::format("tool_{:02}", i)));
    s.set_builtin(builtin);

    const std::vector<std::tuple<tools::Section, const char*, std::optional<tools::Event>>> steps{
        {tools::Section::Builtin, "Show all 20", std::nullopt},
        {tools::Section::Servers, "Start", ti::Server{"browser", tools::ServerAction::Start}},
        {tools::Section::Servers, "Tools: on",
         ti::SetToolsEnabled{"a", {tools::ToolId::mcp("remote", "remote_bash"), tools::ToolId::mcp("remote", "remote_file")}, false}},
        {tools::Section::Hosts, "Allow for session", ti::AnswerPrompt{9, tools::PromptAnswer::AllowSession}},
        {tools::Section::Peers, "No peers", std::nullopt},
    };
    ToolsView view;
    view.resize(800, 600);
    view.show();
    std::vector<tools::Message> got;
    QObject::connect(&view, &ToolsView::message, [&](const tools::Message& m) { got.push_back(m); });
    for (const auto& [section, label, event] : steps) {
        INFO(label);
        (void)s.update(tools::msg::SectionSelected{section});
        view.render(s);
        QApplication::processEvents();
        got.clear();
        click_label(view, label);
        std::optional<tools::Event> first;
        for (auto& m : got)
            if (auto e = s.update(m).second; e && !first) first = std::move(e);
        CHECK(first == event);
        if (std::string_view(label) == "Show all 20") CHECK(s.block_flag("harness:all", false));
        if (std::string_view(label) == "No peers") CHECK(shows_text(view, "No peers"));
    }
    CHECK(pane(s).id == "tools");
}

TEST_CASE("agents view lists subagents and their controls") {
    app();
    namespace core = loa::core;
    namespace attr = core::attr;
    core::Dom dom;
    auto ins = [&](core::NodeId parent, core::ElementKind kind, std::initializer_list<std::pair<std::string_view, core::Value>> list) {
        core::Attrs a;
        for (const auto& [k, v] : list) a.insert_or_assign(std::string(k), v);
        auto p = dom.commit(core::Draft::insert(parent, kind, std::move(a)), 0);
        REQUIRE(p);
        return p->node;
    };
    const auto session = ins(core::NodeId::DOCUMENT, core::ElementKind::Session,
                             {{attr::ID, "s1"}, {attr::HARNESS, "omp"}, {attr::NODE, "key"}, {attr::CREATED, std::int64_t{0}}});
    const auto turn = ins(session, core::ElementKind::Turn, {{attr::STATUS, "running"}});
    ins(turn, core::ElementKind::Job, {{attr::KIND, core::JobKind::Shell}, {attr::COMMAND, "cargo test"}});
    const auto alpha = ins(turn, core::ElementKind::Job, {{attr::KIND, core::JobKind::Subagent}, {attr::ID, "a1"}, {attr::TITLE, "Alpha"}});
    ins(turn, core::ElementKind::Job, {{attr::KIND, core::JobKind::Subagent}, {attr::ID, "b1"}, {attr::TITLE, "Beta"}, {attr::SESSION, "/tmp/b1.jsonl"}});

    const auto agents_state = agents::State::from_dom(dom, agents::PaneKind::Agents);
    const auto jobs_state = agents::State::from_dom(dom, agents::PaneKind::Jobs);
    AgentsView av, jv;
    av.render(agents_state);
    jv.render(jobs_state);
    std::vector<agents::Message> got;
    QObject::connect(&av, &AgentsView::message, [&](const agents::Message& m) { got.push_back(m); });
    REQUIRE(click_label(av, "Alpha"));
    REQUIRE(got.size() == 1);
    CHECK(std::get<agents::msg::Open>(got[0]).node == alpha);
    CHECK(shows_text(av, "Transcript"));
    CHECK_FALSE(shows_text(av, "cargo test"));
    CHECK_FALSE(shows_text(jv, "Alpha"));
    CHECK(shows_text(jv, "cargo test"));
    CHECK(pane(agents_state).id == "agents");
    CHECK(pane(jobs_state).title == "Background Jobs");
}
