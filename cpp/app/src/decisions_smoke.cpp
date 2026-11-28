// The `decisions` smoke screen: one card of every decision kind on a desk, and the unattended
// settings.

#include <memory>

#include <QHBoxLayout>
#include <QWidget>

#include "loa/core/patch.hpp"
#include "loa/ui/app/notifier.hpp"
#include "loa/ui/app/smoke.hpp"
#include "loa/ui/session/decisions/desk.hpp"
#include "loa/ui/session/decisions/view.hpp"

namespace loa::ui::app {

namespace {

namespace d = loa::ui::session::decisions;
using loa::core::Attrs;
using loa::core::Draft;
using loa::core::ElementKind;
using loa::core::NodeId;

constexpr std::string_view DIFF = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-pub fn old() {}\n+pub fn new() {}\n";

// The desk and DOM outlive the views that render them.
struct Screen : QWidget {
    loa::core::Dom dom;
    std::unique_ptr<d::Desk> desk;
};

bool insert(loa::core::Dom& dom, NodeId parent, ElementKind kind, Attrs attrs, std::string_view text = {}) {
    return dom.commit(Draft::insert(parent, kind, std::move(attrs), std::string(text)), 0).has_value();
}

} // namespace

QWidget* decisions_screen() {
    auto* screen = new Screen;
    screen->setAccessibleName(QStringLiteral("decisions screen"));
    auto& dom = screen->dom;
    const NodeId session{1}, turn{2}, call{3};
    bool ok = insert(dom, NodeId::DOCUMENT, ElementKind::Session, {{"id", "s1"}, {"harness", "omp"}, {"node", "k"}, {"created", 1}});
    ok = ok && insert(dom, session, ElementKind::Turn, {});
    ok = ok && insert(dom, turn, ElementKind::Call, {{"tool", "bash"}});
    ok = ok && insert(dom, call, ElementKind::Decision, {{"kind", "approval"}}, DIFF);
    ok = ok && insert(dom, turn, ElementKind::Decision, {{"kind", "question"}, {"options", std::vector<std::string>{"red", "blue"}}}, "Color?");
    ok = ok && insert(dom, turn, ElementKind::Decision, {{"kind", "plan"}}, "1. **Port** the cards\n2. Test");
    ok = ok && insert(dom, turn, ElementKind::Decision,
                      {{"kind", "form"}, {"fields", R"({"properties":{"env":{"enum":["dev","prod"]},"n":{"type":"integer"}},"required":["env"]})"}},
                      "Deploy where?");
    if (!ok) return nullptr;
    screen->desk = std::make_unique<d::Desk>("me", d::UnattendedView{}, std::make_shared<DesktopNotifier>("omp-gui"));
    (void)screen->desk->sync(dom, 1);
    screen->desk->set_active("s1");

    auto* row = new QHBoxLayout(screen);
    auto* cards = new d::DecisionsView;
    cards->setObjectName(QStringLiteral("decision cards"));
    auto* settings = new d::UnattendedSettingsView;
    settings->setObjectName(QStringLiteral("unattended settings"));
    cards->render(*screen->desk->session("s1"));
    settings->render(screen->desk->policy());
    QObject::connect(cards, &d::DecisionsView::message, cards, [screen, cards](const d::Message& m) {
        (void)screen->desk->update(d::desk_msg::Card{"s1", m});
        cards->render(*screen->desk->session("s1"));
    });
    QObject::connect(settings, &d::UnattendedSettingsView::message, settings, [screen, settings](const d::unattended::Message& m) {
        (void)screen->desk->update(d::desk_msg::Settings{m});
        settings->render(screen->desk->policy());
    });
    row->addWidget(cards, 2);
    row->addWidget(settings, 1);
    screen->resize(1100, 900);
    return screen;
}

std::string check_decisions_screen(QWidget& screen) {
    const auto* cards = screen.findChild<QWidget*>(QStringLiteral("decision cards"));
    if (!cards) return "missing decision cards";
    if (cards->findChildren<d::CardView*>().size() != 4) return "expected one card per decision kind";
    if (!screen.findChild<QWidget*>(QStringLiteral("diffPreview"))) return "approval diff preview not rendered";
    if (!screen.findChild<QWidget*>(QStringLiteral("unattended settings"))) return "missing unattended settings";
    return {};
}

} // namespace loa::ui::app
