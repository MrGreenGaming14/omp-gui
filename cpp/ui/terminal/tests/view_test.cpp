// The view renders offscreen and reports input as Messages (the simulator half of
// `tabs_follow_titles_selection_closing_and_failures`).

#include <doctest/doctest.h>

#include <filesystem>

#include <QApplication>
#include <QClipboard>
#include <QKeyEvent>
#include <QLabel>
#include <QPushButton>

#include "loa/ui/terminal/view.hpp"

using namespace loa::ui::terminal;

namespace {
QApplication& app() {
    static int argc = 1;
    static char name[] = "loa_ui_terminal_tests";
    static char* argv[] = {name, nullptr};
    static auto* a = new QApplication(argc, argv);
    return *a;
}
Bytes b(std::string_view s) { return {s.begin(), s.end()}; }
} // namespace

TEST_CASE("tab titles select, the canvas reports keys and the clipboard round-trips") {
    app();
    State s(default_metrics(), LocalShell::login_shell(std::filesystem::temp_directory_path()));
    TerminalView view;
    std::vector<Message> got;
    QObject::connect(&view, &TerminalView::message, [&](const Message& m) { got.push_back(m); });
    view.resize(640, 400);
    view.render(s);
    view.show();
    QApplication::processEvents();
    bool empty = false;
    for (auto* w : view.findChildren<QWidget*>())
        if (w->accessibleName() == QStringLiteral("no terminal")) empty = true;
    CHECK(empty);

    const auto a = s.open(Remote{"a"}).first;
    const auto bt = s.open(Remote{"b"}).first;
    (void)s.update(msg::Output{bt, ChannelRead{port::channel_read::Data{b("\x1b]0;vim\x07last words")}}});
    (void)s.update(msg::Select{a});
    view.render(s);
    QApplication::processEvents();
    auto* canvas = view.findChild<Canvas*>();
    REQUIRE(canvas);
    CHECK(canvas->isVisible());
    bool status = false;
    for (auto* l : view.findChildren<QLabel*>())
        if (l->text() == QString::fromUtf8("Connecting to a\u2026")) status = true;
    CHECK(status);

    QPushButton* vim = nullptr;
    for (auto* p : view.findChildren<QPushButton*>())
        if (p->text() == QStringLiteral("vim")) vim = p;
    REQUIRE_MESSAGE(vim, "tab title shown");
    got.clear();
    vim->click();
    REQUIRE(!got.empty());
    const auto* select = std::get_if<msg::Select>(&got.back());
    REQUIRE(select);
    CHECK(select->tab == bt);

    got.clear();
    QKeyEvent press(QEvent::KeyPress, Qt::Key_L, Qt::NoModifier, QStringLiteral("l"));
    QApplication::sendEvent(canvas, &press);
    REQUIRE(got.size() == 1);
    const auto* key = std::get_if<msg::Canvas>(&got[0]);
    REQUIRE(key);
    const auto* k = std::get_if<canvas_event::KeyPress>(&key->event);
    REQUIRE(k);
    CHECK(k->key == Key::character("l"));
    CHECK(k->text == std::optional<std::string>("l"));

    got.clear();
    view.apply(Task{{op::WriteClipboard{"copied", false}, op::ReadClipboard{bt}}});
    REQUIRE(got.size() == 1);
    const auto* paste = std::get_if<msg::Paste>(&got[0]);
    REQUIRE(paste);
    CHECK(paste->text == std::optional<std::string>("copied"));
}
