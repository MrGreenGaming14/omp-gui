// Every pane renders offscreen and reports input as Messages (the Rust `every_pane_renders`).

#include <doctest/doctest.h>

#include <QApplication>
#include <QPushButton>

#include "loa/ui/panel/view.hpp"

using namespace loa::ui::panel;

namespace {
QApplication& app() {
    static int argc = 1;
    static char name[] = "loa_ui_panel_tests";
    static char* argv[] = {name, nullptr};
    static auto* a = qApp ? static_cast<QApplication*>(qApp) : new QApplication(argc, argv);
    return *a;
}

struct FakeFs : files::FileSystem {
    std::expected<std::vector<files::Entry>, std::string> list(const files::Path&) const override {
        return std::vector<files::Entry>{{"src", true}, {"a.rs", false}};
    }
};
} // namespace

TEST_CASE("every pane renders") {
    app();
    diff::State d;
    auto parsed = diff::parse_unified("--- a\n+++ a\n@@ -1 +1,2 @@\n-x\n+y\n+z\n--- b\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\n");
    REQUIRE(parsed);
    (void)d.set_files(std::move(*parsed));
    (void)d.update(diff::msg::SetMode{diff::Mode::Split});
    session_tree::State tree;
    tree.set_family({{.id = "s", .title = "Main"}}, "s");
    files::State f("/r", std::make_shared<FakeFs>());

    DiffView dv;
    dv.resize(360, 400);
    dv.render(d);
    dv.show();
    FilesView fv;
    fv.render(f);
    fv.show();
    SessionTreeView tv;
    tv.render(tree);
    tv.show();
    QApplication::processEvents();
    CHECK(std::vector{pane(d).id, pane(f).id, pane(tree).id} == std::vector<std::string_view>{"diff", "files", "session"});

    std::optional<files::Message> got;
    QObject::connect(&fv, &FilesView::message, [&](const files::Message& m) { got = m; });
    for (auto* b : fv.findChildren<QPushButton*>())
        if (b->accessibleName() == "a.rs") b->click();
    REQUIRE(got);
    CHECK(std::get<files::msg::Click>(*got).node == 2);
}
