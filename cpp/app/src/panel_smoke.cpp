// The `panel` smoke screen: the right panel's diff, files and session tree panes side by side.

#include <filesystem>
#include <memory>

#include <QHBoxLayout>
#include <QWidget>

#include "loa/ui/app/local_fs.hpp"
#include "loa/ui/app/smoke.hpp"
#include "loa/ui/panel/view.hpp"

namespace loa::ui::app {

namespace {

constexpr std::string_view SAMPLE = "diff --git a/src/lib.rs b/src/lib.rs\n"
                                    "--- a/src/lib.rs\n"
                                    "+++ b/src/lib.rs\n"
                                    "@@ -10,2 +10,3 @@ fn compute() {\n"
                                    "     let a = 1;\n"
                                    "-    let b = 2;\n"
                                    "+    let b = 3;\n"
                                    "+    let c = 4;\n"
                                    "diff --git a/added.txt b/added.txt\n"
                                    "new file mode 100644\n"
                                    "--- /dev/null\n"
                                    "+++ b/added.txt\n"
                                    "@@ -0,0 +1,1 @@\n"
                                    "+first\n";

// The pane states outlive the views that render them.
struct Screen : QWidget {
    panel::diff::State diff;
    std::unique_ptr<panel::files::State> files;
    panel::session_tree::State tree;
};

} // namespace

QWidget* panel_screen() {
    auto* screen = new Screen;
    screen->setAccessibleName(QStringLiteral("panel"));
    auto parsed = panel::diff::parse_unified(SAMPLE);
    if (!parsed) return nullptr;
    (void)screen->diff.set_files(std::move(*parsed));
    screen->files = std::make_unique<panel::files::State>(std::filesystem::current_path(), std::make_shared<LocalFs>());
    screen->tree.set_family({{.id = "s", .title = "Main", .turns = {{{1}, 1, "Hello"}}}, {.id = "f", .parent = "s", .forked_at = 1, .title = "Fork"}}, "s");
    auto* row = new QHBoxLayout(screen);
    auto* dv = new panel::DiffView;
    dv->render(screen->diff);
    QObject::connect(dv, &panel::DiffView::message, dv, [screen, dv](const panel::diff::Message& m) {
        auto [task, _] = screen->diff.update(m);
        dv->render(screen->diff);
        dv->apply(task);
    });
    auto* fv = new panel::FilesView;
    fv->render(*screen->files);
    QObject::connect(fv, &panel::FilesView::message, fv, [screen, fv](const panel::files::Message& m) {
        (void)screen->files->update(m);
        fv->render(*screen->files);
    });
    auto* tv = new panel::SessionTreeView;
    tv->render(screen->tree);
    QObject::connect(tv, &panel::SessionTreeView::message, tv, [screen, tv](const panel::session_tree::Message& m) {
        (void)screen->tree.update(m);
        tv->render(screen->tree);
    });
    row->addWidget(dv);
    row->addWidget(fv);
    row->addWidget(tv);
    screen->resize(1080, 480);
    return screen;
}

std::string check_panel_screen(QWidget& screen) {
    if (!screen.findChild<panel::DiffView*>()) return "missing diff view";
    for (const auto* name : {"diff body", "files list", "session tree list"}) {
        bool found = false;
        for (const auto* w : screen.findChildren<QWidget*>())
            if (w->accessibleName() == QString::fromUtf8(name) && w->isVisible()) found = true;
        if (!found) return std::string("missing ") + name;
    }
    return {};
}

} // namespace loa::ui::app
