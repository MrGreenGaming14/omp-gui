// The `transcript` smoke screen: a sample session with every row kind, a streamed markdown reply
// and a decision slot, rendered by the transcript view.

#include <QLabel>
#include <QVBoxLayout>
#include <QWidget>

#include "loa/core/dom.hpp"
#include "loa/ui/app/smoke.hpp"
#include "loa/ui/session/transcript/view.hpp"

namespace loa::ui::app {

namespace {

namespace core = loa::core;
namespace tr = session::transcript;
using K = core::ElementKind;

// The DOM and state outlive the view that renders them.
struct Screen : QWidget {
    core::Dom dom;
    tr::State state;
    tr::TranscriptView* view = nullptr;

    core::NodeId add(core::NodeId parent, K kind, core::Attrs attrs = {}, std::string text = {}) {
        return commit(core::Draft::insert(parent, kind, std::move(attrs), std::move(text)));
    }
    core::NodeId commit(core::Draft d) {
        auto patch = dom.commit(std::move(d), 0);
        if (!patch) return core::NodeId::DOCUMENT;
        state.apply(*patch, dom);
        return patch->node;
    }
};

} // namespace

QWidget* transcript_screen() {
    auto* screen = new Screen;
    screen->setAccessibleName(QStringLiteral("transcript screen"));
    const auto session = screen->add(core::NodeId::DOCUMENT, K::Session, {{"id", "smoke"}, {"harness", "omp"}, {"node", "n1"}, {"created", 0}});
    const auto t = screen->add(session, K::Turn);
    screen->add(t, K::Message, {{"role", "user"}}, "Summarise the build failure");
    const auto read = screen->add(t, K::Call, {{"tool", "read"}, {"status", "done"}, {"started", 0}, {"ended", 1200}});
    screen->add(read, K::Result, {}, "fn main() {}");
    screen->add(t, K::Message, {{"role", "assistant"}, {"kind", "thinking"}, {"status", "done"}}, "weighing");
    const auto bash = screen->add(t, K::Call, {{"tool", "bash"}, {"title", "cargo test"}, {"status", "running"}});
    screen->add(bash, K::Decision, {{"kind", "approval"}}, "rm -rf target");
    screen->add(t, K::Ext, {{"harness", "omp"}, {"type", "notice"}}, "{}");
    screen->add(session, K::Ext, {{"harness", "omp"}, {"type", std::string(tr::COMMANDS_EXT)}}, "{}");
    const auto reply = screen->add(t, K::Message, {{"role", "assistant"}, {"status", "streaming"}});
    for (const auto* chunk : {"## Cause\n\n", "The **linker** failed on `libfoo`.\n\n", "- one\n- two\n\n", "```sh\ncargo build\n```\n"})
        screen->commit(core::Draft::append(reply, chunk));
    screen->add(session, K::Todo, {{"state", "in_progress"}}, "Fix the linker flags");
    auto* layout = new QVBoxLayout(screen);
    layout->setContentsMargins(0, 0, 0, 0);
    screen->view = new tr::TranscriptView;
    screen->view->set_slot([](tr::Slot slot) {
        auto* card = new QLabel(QStringLiteral("decision card %1").arg(slot.node.value));
        card->setAccessibleName(QStringLiteral("decision card"));
        return card;
    });
    auto* view = screen->view;
    QObject::connect(view, &tr::TranscriptView::message, view, [screen, view](const tr::Message& m) {
        auto [task, _] = screen->state.update(m);
        view->render(screen->state, screen->dom);
        view->apply(task);
    });
    layout->addWidget(view);
    screen->resize(900, 640);
    view->render(screen->state, screen->dom);
    return screen;
}

std::string check_transcript_screen(QWidget& screen) {
    auto* view = screen.findChild<tr::TranscriptView*>();
    if (!view) return "missing transcript view";
    if (view->list()->rows() == 0) return "no rows built";
    if (!screen.findChild<QWidget*>(QStringLiteral("transcript-code"))) return "missing code block";
    bool card = false, copy = false;
    for (const auto* w : screen.findChildren<QWidget*>()) {
        card |= w->accessibleName() == QStringLiteral("decision card");
        copy |= w->accessibleName() == QStringLiteral("Copy code block");
    }
    if (!card) return "missing decision slot";
    if (!copy) return "missing copy button";
    return {};
}

} // namespace loa::ui::app
