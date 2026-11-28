// The `agents` smoke screen: the agents and background jobs panes over a sample session DOM,
// beside the tools page.

#include <QAbstractButton>
#include <QHBoxLayout>
#include <QLabel>
#include <QWidget>

#include "loa/ui/app/smoke.hpp"
#include "loa/ui/panel/agents_view.hpp"
#include "loa/ui/panel/tools_view.hpp"

namespace loa::ui::app {

namespace {

namespace core = loa::core;
namespace attr = core::attr;

// The pane states outlive the views that render them.
struct Screen : QWidget {
    core::Dom dom;
    panel::agents::State agents = panel::agents::State::agents();
    panel::agents::State jobs = panel::agents::State::background_jobs();
    panel::tools::State tools;
};

bool build_dom(core::Dom& dom) {
    auto ins = [&](core::NodeId parent, core::ElementKind kind, core::Attrs a, std::string text = {}) -> std::optional<core::NodeId> {
        auto p = dom.commit(core::Draft::insert(parent, kind, std::move(a), std::move(text)), 0);
        if (!p) return std::nullopt;
        return p->node;
    };
    auto session = ins(core::NodeId::DOCUMENT, core::ElementKind::Session,
                       {{std::string(attr::ID), "s1"}, {std::string(attr::HARNESS), "omp"}, {std::string(attr::NODE), "key"}, {std::string(attr::CREATED), std::int64_t{0}}});
    if (!session) return false;
    auto turn = ins(*session, core::ElementKind::Turn, {{std::string(attr::STATUS), "running"}});
    if (!turn) return false;
    return ins(*turn, core::ElementKind::Job,
               {{std::string(attr::KIND), core::JobKind::Subagent}, {std::string(attr::ID), "a1"}, {std::string(attr::TITLE), "Explore the repo"},
                {std::string(attr::SESSION), "/tmp/a1.jsonl"}, {std::string(attr::STARTED), std::int64_t{1000}}},
               "reading crates\nfound 27 crates\n")
               .has_value() &&
           ins(*turn, core::ElementKind::Job,
               {{std::string(attr::KIND), core::JobKind::Shell}, {std::string(attr::COMMAND), "cargo build"}, {std::string(attr::STATUS), "done"},
                {std::string(attr::EXIT_CODE), std::int64_t{1}}},
               "error: build failed\n")
               .has_value() &&
           ins(*turn, core::ElementKind::Call, {{std::string(attr::TOOL), "wait"}, {std::string(attr::STATUS), "running"}}).has_value();
}

} // namespace

QWidget* agents_screen() {
    auto* screen = new Screen;
    screen->setAccessibleName(QStringLiteral("agents"));
    if (!build_dom(screen->dom)) {
        delete screen;
        return nullptr;
    }
    screen->agents.reset(screen->dom);
    screen->jobs.reset(screen->dom);
    (void)screen->agents.update(panel::agents::msg::Tick{61'000});
    namespace tl = panel::tools;
    tl::BuiltinTools builtin{"omp", {{"bash", "Run a shell command", {}}, {"read", "Read a file", {}}}, {{"project_search", "Search projects", {}}}, std::nullopt};
    screen->tools.set_builtin(builtin);
    screen->tools.set_servers({{"remote", loa::contracts::McpStdio{"loa-remote", {}, {}}, tl::server_status::Running{}, tl::ServerKind::AppHosted,
                                {{"remote_bash", "Run a command on a host", {}}}}});
    screen->tools.set_projects({{"p", "Example"}});
    screen->tools.select_project("p");

    auto* row = new QHBoxLayout(screen);
    auto* av = new panel::AgentsView;
    auto* jv = new panel::AgentsView;
    auto* tv = new panel::ToolsView;
    for (auto [view, state] : {std::pair{av, &screen->agents}, std::pair{jv, &screen->jobs}}) {
        view->render(*state);
        QObject::connect(view, &panel::AgentsView::message, view, [view, state](const panel::agents::Message& m) {
            (void)state->update(m);
            view->render(*state);
        });
    }
    tv->render(screen->tools);
    QObject::connect(tv, &panel::ToolsView::message, tv, [screen, tv](const tl::Message& m) {
        (void)screen->tools.update(m);
        tv->render(screen->tools);
    });
    row->addWidget(av);
    row->addWidget(jv);
    row->addWidget(tv, 2);
    screen->resize(1200, 520);
    return screen;
}

std::string check_agents_screen(QWidget& screen) {
    auto has = [&](const char* text) {
        const auto want = QString::fromUtf8(text);
        for (const auto* l : screen.findChildren<QLabel*>())
            if (l->text() == want && l->isVisible()) return true;
        for (const auto* b : screen.findChildren<QAbstractButton*>())
            if ((b->text() == want || b->accessibleName() == want) && b->isVisible()) return true;
        return false;
    };
    for (const auto* text : {"Explore the repo", "Transcript", "cargo build", "exit 1", "Built in", "MCP servers", "bash"})
        if (!has(text)) return std::string("missing ") + text;
    return {};
}

} // namespace loa::ui::app
