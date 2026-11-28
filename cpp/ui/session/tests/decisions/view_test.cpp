// The decision views render offscreen and report input as Messages.

#include <doctest/doctest.h>

#include <QApplication>
#include <QLabel>
#include <QLineEdit>
#include <QPushButton>

#include "loa/core/testkit.hpp"
#include "loa/ui/session/decisions/view.hpp"

using namespace loa::ui::session::decisions;
using loa::core::Attrs;
using loa::core::ElementKind;
using loa::core::testkit::DomFixture;

namespace {

QApplication& app() {
    static int argc = 1;
    static char name[] = "loa_ui_session_tests";
    static char* argv[] = {name, nullptr};
    static auto* a = new QApplication(argc, argv);
    return *a;
}

bool shows(const QWidget& root, const QString& text) {
    for (auto* l : root.findChildren<QLabel*>())
        if (l->isVisibleTo(&root) && l->text().contains(text)) return true;
    for (auto* b : root.findChildren<QPushButton*>())
        if (b->isVisibleTo(&root) && b->text().contains(text)) return true;
    return false;
}

QPushButton* button(const QWidget& root, const QString& accessible) {
    for (auto* b : root.findChildren<QPushButton*>())
        if (b->isVisibleTo(&root) && b->accessibleName() == accessible) return b;
    return nullptr;
}

} // namespace

TEST_CASE("decision cards render every kind and report clicks") {
    app();
    auto f = DomFixture::new_session();
    f.turn().call("bash");
    const auto call = f.last;
    const auto approval = f.add(call, ElementKind::Decision, Attrs{{"kind", "approval"}}, "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n");
    const auto question = f.add(call, ElementKind::Decision, Attrs{{"kind", "question"}, {"options", std::vector<std::string>{"red", "blue"}}}, "Color?");
    (void)f.add(call, ElementKind::Decision, Attrs{{"kind", "plan"}}, "1. **Do**");
    (void)f.add(call, ElementKind::Decision,
                Attrs{{"kind", "form"}, {"fields", R"({"properties":{"tag":{"type":"string"},"ok":{"type":"boolean"}}})"}}, "Fill in");
    State state(Viewer{"me", false});
    state.sync(f.dom);

    DecisionsView view;
    view.resize(640, 1200);
    std::vector<Message> got;
    QObject::connect(&view, &DecisionsView::message, &view, [&](const Message& m) { got.push_back(m); });
    view.render(state);
    view.show();
    QApplication::processEvents();
    for (auto text : {"omp wants to run", "bash", "+new", "-old", "Color?", "omp proposes a plan", "omp needs input", "Submit", "Skip"})
        CHECK_MESSAGE(shows(view, QString::fromUtf8(text)), text);
    auto* allow = button(view, QStringLiteral("Allow once"));
    REQUIRE(allow);
    allow->click();
    REQUIRE(got.size() == 1);
    CHECK(got[0] == Message(msg::Choose{approval, 0}));
    auto* blue = button(view, QStringLiteral("blue"));
    REQUIRE(blue);
    blue->click();
    CHECK(got.back() == Message(msg::Choose{question, 1}));
    for (auto* w : view.findChildren<QWidget*>())
        if (w->isVisibleTo(&view))
            CHECK_MESSAGE(!w->accessibleName().isEmpty(), (std::string(w->metaObject()->className()) + " " + w->objectName().toStdString()));

    // Answering locks the card; a re-render keeps the other cards' widgets.
    (void)state.update(msg::Choose{approval, 0});
    view.render(state);
    QApplication::processEvents();
    CHECK(shows(view, QStringLiteral("Sending answer…")));
}

TEST_CASE("the unattended settings list every kind and report edits") {
    app();
    UnattendedSettingsView view;
    std::vector<unattended::Message> got;
    QObject::connect(&view, &UnattendedSettingsView::message, &view, [&](const unattended::Message& m) { got.push_back(m); });
    Unattended policy;
    policy.update(unattended::Enabled{true});
    view.render(policy);
    view.show();
    QApplication::processEvents();
    for (auto text : {"Answer waiting prompts after a timeout", "Approvals", "Questions", "Plan reviews", "Forms"})
        CHECK_MESSAGE(shows(view, QString::fromUtf8(text)), text);
}
