// The views render offscreen and report input as messages: the launcher opens a typed URL (the
// Rust `the_launcher_view_opens_a_typed_url`), and the pixel viewer takes clicks and typing.

#include <doctest/doctest.h>

#include <QApplication>
#include <QLineEdit>
#include <QMouseEvent>

#include "loa/runtime/tempfile.hpp"
#include "loa/ui/kit/testkit.hpp"
#include "loa/ui/surface/view.hpp"
#include "support.hpp"

using namespace loa::ui::surface;
namespace vs = loa::view::surface;

namespace {

QApplication& app() {
    static int argc = 1;
    static char name[] = "loa_ui_surface_tests";
    static char* argv[] = {name, nullptr};
    static auto* a = qApp ? static_cast<QApplication*>(qApp) : new QApplication(argc, argv);
    return *a;
}

QLineEdit* field(QWidget& root, const QString& name) {
    for (auto* e : root.findChildren<QLineEdit*>())
        if (e->accessibleName() == name) return e;
    return nullptr;
}

} // namespace

TEST_CASE("the launcher view opens a typed URL") {
    app();
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    Panel panel(support::options(), vs::browser_status::Ready{"/usr/bin/chromium", true}, dir->path());
    PanelView view;
    view.resize(420, 900);
    std::vector<Intent> intents;
    QObject::connect(&view, &PanelView::message, &view, [&](const panel::Message& m) {
        if (auto e = panel.update(m).second) intents.push_back(*e);
        view.render(panel);
    });
    view.render(panel);
    view.show();
    auto* url = field(view, QStringLiteral("Search or enter a URL"));
    REQUIRE(url != nullptr);
    url->setFocus();
    for (char c : std::string("example.com")) loa::ui::kit::testkit::type_char(*url, std::string(1, c));
    loa::ui::kit::testkit::key(*url, Qt::Key_Return, 0, "\r");
    CHECK(intents == std::vector<Intent>{vs::intent::OpenBrowser{"example.com"}});
    CHECK(panel.pending() == "Opening example.com");
}

TEST_CASE("the pixel viewer takes clicks and typing once focused") {
    app();
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    Panel panel(support::options(), vs::browser_status::Missing{}, dir->path());
    vs::SurfaceInfo info{support::uri("fake", "one"), "Fake", 4, 3, vs::SurfaceMode::Pixel, std::nullopt};
    auto surface = std::make_shared<support::FakeSurface>(info, std::nullopt);
    PanelView view;
    view.resize(400, 340);
    std::vector<vs::Input> sent;
    const auto apply = [&](const panel::Message& m) {
        auto [task, _] = panel.update(m);
        for (const auto& w : task.work)
            if (const auto* s = std::get_if<work::Send>(&w.work)) sent.push_back(s->event.input);
        view.render(panel);
    };
    QObject::connect(&view, &PanelView::message, &view, apply);
    (void)panel.attach(surface, "browser");
    auto f = std::make_shared<const vs::Frame>(vs::Frame{1, 0, 0, 4, 3, {0, 0, 4, 3}, vs::Encoding::Rgba, true, support::solid(4, 3, {250, 10, 10, 255})});
    apply(panel::msg::Surface{info.uri, msg::Frame{f}});
    view.show();
    QApplication::processEvents();
    auto* viewer = view.findChild<Viewer*>();
    REQUIRE(viewer != nullptr);
    REQUIRE(viewer->isVisible());
    const QPointF at(viewer->width() / 2.0, viewer->height() / 2.0);
    QMouseEvent press(QEvent::MouseButtonPress, at, viewer->mapToGlobal(at), Qt::LeftButton, Qt::LeftButton, Qt::NoModifier);
    QApplication::sendEvent(viewer, &press);
    QMouseEvent release(QEvent::MouseButtonRelease, at, viewer->mapToGlobal(at), Qt::LeftButton, Qt::NoButton, Qt::NoModifier);
    QApplication::sendEvent(viewer, &release);
    CHECK(panel.surface(info.uri)->pixel()->focused());
    loa::ui::kit::testkit::type_char(*viewer, "h");
    loa::ui::kit::testkit::type_char(*viewer, "i");
    std::size_t presses = 0;
    std::vector<vs::Input> typed;
    for (const auto& i : sent) {
        if (std::holds_alternative<input::MouseButton>(i)) ++presses;
        if (std::holds_alternative<input::Text>(i)) typed.push_back(i);
    }
    CHECK(presses == 2);
    CHECK(typed == std::vector<vs::Input>{input::Text{"h"}, input::Text{"i"}});
    // The local cursor sits on the picture.
    const auto cursor = panel.surface(info.uri)->pixel()->cursor();
    REQUIRE(cursor);
    CHECK(cursor->on_surface.has_value());
}

TEST_CASE("document tabs render their pages and zoom from the header") {
    app();
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    Panel panel(support::options(), vs::browser_status::Missing{}, dir->path());
    vs::SurfaceInfo info{support::uri("docs", "1"), "Pages", 300, 200, vs::SurfaceMode::Document, std::string(support::PAGED_MIME)};
    auto surface = std::make_shared<support::FakeSurface>(info, std::nullopt);
    PanelView view;
    view.resize(400, 400);
    QObject::connect(&view, &PanelView::message, &view, [&](const panel::Message& m) {
        (void)panel.update(m);
        view.render(panel);
    });
    (void)panel.attach(surface, "docs");
    (void)panel.update(panel::msg::Surface{info.uri, msg::Document{std::make_shared<const vs::Document>(vs::Document{"p", std::string(support::PAGED_MIME), {}})}});
    view.render(panel);
    view.show();
    CHECK(loa::ui::kit::testkit::click_label(view, "+"));
    CHECK(panel.surface(info.uri)->document()->zoom() == 1.25F);
    CHECK(loa::ui::kit::testkit::click_label(view, "125%"));
    CHECK(panel.surface(info.uri)->document()->zoom() == 1.0F);
}
