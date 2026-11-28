// The settings smoke screens: the project and New project screens, the agent definitions form
// and the workflow editor, each over sample view models and wired to its own reducer.

#include <QAbstractButton>
#include <QLabel>
#include <QLineEdit>
#include <QVBoxLayout>
#include <QWidget>

#include "loa/ui/app/smoke.hpp"
#include "loa/ui/settings/agents/view.hpp"
#include "loa/ui/settings/projects/view.hpp"
#include "loa/ui/settings/workflows/view.hpp"

namespace loa::ui::app {

namespace {

namespace projects = settings::projects;
namespace agents = settings::agents;
namespace workflows = settings::workflows;

// A screen state owned by the widget that renders it, re-rendered after every message.
template <class S, class V, class M> struct Holder : QWidget {
    S state;
    V* view;
    template <class... A> explicit Holder(S s, A&&... args) : state(std::move(s)), view(new V(std::forward<A>(args)...)) {
        auto* l = new QVBoxLayout(this);
        l->setContentsMargins(0, 0, 0, 0);
        l->addWidget(view);
        view->render(state);
        QObject::connect(view, &V::message, view, [this](const M& m) {
            (void)state.update(m);
            view->render(state);
        });
        resize(1100, 720);
    }
};

bool shows(QWidget& root, const char* text) {
    const auto want = QString::fromUtf8(text);
    for (const auto* l : root.findChildren<QLabel*>())
        if (l->text() == want && l->isVisible()) return true;
    for (const auto* b : root.findChildren<QAbstractButton*>())
        if ((b->text() == want || b->accessibleName() == want) && b->isVisible()) return true;
    return false;
}

std::string expect(QWidget& root, std::initializer_list<const char*> texts) {
    for (const auto* t : texts)
        if (!shows(root, t)) return std::string("missing ") + t;
    return {};
}

} // namespace

QWidget* settings_projects_screen() {
    using namespace projects;
    State s("Example");
    ProjectOverview o;
    o.cwd = "/home/user/example";
    o.settings.instructions = "Use rustls.";
    o.settings.pinned = {PinnedFile{"docs/ARCHITECTURE.md", "the contract"}};
    o.ideas = std::string("- try iroh relays\n");
    SessionRow row;
    row.id = "a";
    row.title = "Set up iroh";
    row.harness = "omp";
    row.turns = 3;
    row.digest_state = DigestState::Fresh;
    row.digest = loa::view::settings::projects::Digest{"Picked port 31415.", {"Use QUIC", "Keep TCP fallback"}};
    o.sessions = {row};
    s.set_overview(std::move(o), 3 * 3'600'000);
    s.set_omp_history(loa::view::settings::projects::omp_history::Ready{{{"native", "Terminal conversation", 1'000}}});
    auto* screen = new Holder<State, ProjectView, Message>(std::move(s));
    screen->setAccessibleName(QStringLiteral("projects"));

    auto* fresh = new Holder<create::State, CreateView, create::Message>(create::State("/home/user"));
    fresh->setObjectName(QStringLiteral("new project screen"));
    screen->layout()->addWidget(fresh);
    return screen;
}

std::string check_settings_projects_screen(QWidget& screen) {
    if (auto e = expect(screen, {"Shared instructions", "Pinned files", "Sessions", "Set up iroh", "Terminal conversation", "New chat",
                                 "Delete project", "New project", "Create project"});
        !e.empty())
        return e;
    const auto id = QString::fromUtf8(projects::create::NAME_ID.data(), static_cast<qsizetype>(projects::create::NAME_ID.size()));
    if (!screen.findChild<QLineEdit*>(id)) return "missing the New project name input";
    return {};
}

QWidget* settings_agents_screen() {
    using namespace agents;
    State s;
    AgentFields reviewer{"reviewer", "Reviews code", ModelSpec{std::string("anthropic/claude-sonnet")}, std::string("high"),
                         std::vector<std::string>{"read"}, std::nullopt, "You review code.\n"};
    s.set_agents({AgentEntry{"reviewer.md", AgentSource::User, true, reviewer}});
    s.set_choices(Choices{{"anthropic/claude-sonnet", "@slow"}, {"off", "low", "high"}, {"read", "grep", "edit", "bash"}, std::nullopt});
    s.copy(0);
    auto* screen = new Holder<State, AgentsView, Message>(std::move(s));
    screen->setAccessibleName(QStringLiteral("agent definitions"));
    return screen;
}

std::string check_settings_agents_screen(QWidget& screen) {
    return expect(screen, {"Agents", "New agent", "reviewer", "Copy of reviewer", "Template", "Model", "Thinking level", "Tools", "May spawn",
                           "Prompt", "Save", "Close"});
}

QWidget* settings_workflows_screen() {
    using namespace workflows;
    namespace rules = loa::view::settings::workflows::rules;
    State s;
    ManagedFields fields;
    fields.description = "Find context first";
    fields.steps = {"Search", "Summarize"};
    fields.blocks = {RuleBlock{"Guidance", "Be brief."}};
    s.set_workflows({Workflow{"RESEARCH", {"provider/model:high"}, rules::Managed{"Research", {"scout"}, {"builder"}, "Notes", fields}},
                     Workflow{"DEEP", {}, rules::HandWritten{"## Deep Mode\n"}}});
    s.set_agents({AgentInfo{"scout", true}, AgentInfo{"builder", true}, AgentInfo{"critic", false}});
    s.edit("RESEARCH");
    auto* screen = new Holder<State, WorkflowsView, Message>(std::move(s));
    screen->setAccessibleName(QStringLiteral("workflows"));
    return screen;
}

std::string check_settings_workflows_screen(QWidget& screen) {
    return expect(screen, {"Workflows", "New workflow", "RESEARCH", "DEEP", "Routing note", "Step 2", "Add step", "Rule blocks", "Add block",
                           "critic (disabled)", "Save", "Delete"});
}

} // namespace loa::ui::app
