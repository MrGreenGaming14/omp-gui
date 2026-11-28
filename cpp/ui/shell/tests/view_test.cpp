// The Qt views render the state and report input as Messages (offscreen).

#include <doctest/doctest.h>

#include <QApplication>
#include <QKeyEvent>
#include <QLabel>
#include <QLineEdit>
#include <QPushButton>

#include "loa/ui/shell/view.hpp"

using namespace loa::ui::shell;
namespace is = loa::view::shell::sidebar_intent;
namespace in = loa::view::shell::intent;

namespace {
QApplication& app() {
    static int argc = 1;
    static char name[] = "loa_ui_shell_tests";
    static char* argv[] = {name, nullptr};
    // Qt's input devices outlive static QApplication destruction; follow the kit test fixture.
    static auto* a = new QApplication(argc, argv);
    return *a;
}

template <class W> W* find_text(QWidget& root, const QString& text) {
    for (auto* w : root.findChildren<W*>())
        if ((w->text().contains(text) || w->accessibleName().contains(text)) && w->isVisibleTo(&root)) return w;
    return nullptr;
}
} // namespace

TEST_CASE("the frame renders a narrow breadcrumb and reports clicks") {
    app();
    auto state = layout::State::make({}).first;
    state.sidebar_mut().set_nodes({{"k-box", "buildbox", true, false}});
    loa::view::shell::SessionSummary s;
    s.id = "s1";
    s.title = "Ship it";
    s.project = "loa";
    s.node = "k-box";
    s.waiting = true;
    state.sidebar_mut().set_sessions({s});
    state.sidebar_mut().set_projects({"loa"});
    state.set_tabs({{"diff", "Diff", std::nullopt}});
    (void)state.update(layout::msg::Sidebar{sidebar::msg::OpenSession{"s1"}});

    ShellView view;
    std::vector<layout::Message> got;
    QObject::connect(&view, &ShellView::message, [&](const layout::Message& m) { got.push_back(m); });
    view.resize(700, 500);
    view.render(state);
    view.show();
    QApplication::processEvents();

    CHECK(find_text<QLabel>(view, "Ship it") != nullptr);
    CHECK(find_text<QLabel>(view, "Needs you") != nullptr);
    auto* top = view.findChild<QWidget*>("top bar");
    REQUIRE(top != nullptr);
    CHECK(top->height() == 52);

    got.clear();
    auto* fresh = find_text<QPushButton>(view, "New session");
    REQUIRE(fresh != nullptr);
    fresh->click();
    REQUIRE(got.size() == 1);
    CHECK(got[0] == layout::Message{layout::msg::Sidebar{sidebar::msg::Emit{is::NewSession{}}}});

    got.clear();
    auto* search = view.findChild<QLineEdit*>(QString::fromLatin1(sidebar::SEARCH_ID.data()));
    REQUIRE(search != nullptr);
    search->setFocus();
    search->setText("");
    emit search->textEdited("ship");
    REQUIRE(!got.empty());
    CHECK(got.back() == layout::Message{layout::msg::Sidebar{sidebar::msg::Search{"ship"}}});
    view.close();
}

TEST_CASE("the palette view lists rows and maps keys to messages") {
    app();
    palette::State state(palette::Platform::Other);
    state.set_commands({palette::Command(palette::CommandId("a"), "Alpha", "S"),
                        palette::Command(palette::CommandId("b"), "Beta", "S").with_enabled(false)});
    PaletteView view;
    std::vector<palette::Message> got;
    QObject::connect(&view, &PaletteView::message, [&](const palette::Message& m) { got.push_back(m); });
    auto [task, ev] = state.update(palette::msg::Open{});
    view.render(state);
    view.apply(task);
    view.show();
    QApplication::processEvents();
    CHECK(view.isVisible());
    auto* alpha = find_text<QPushButton>(view, "Alpha");
    REQUIRE(alpha != nullptr);
    alpha->click();
    REQUIRE(!got.empty());
    CHECK(got.back() == palette::Message{palette::msg::Activate{0}});
    auto* input = view.findChild<QLineEdit*>(QString::fromLatin1(palette::INPUT_ID.data()));
    REQUIRE(input != nullptr);
    QKeyEvent down(QEvent::KeyPress, Qt::Key_Down, Qt::NoModifier);
    QApplication::sendEvent(input, &down);
    CHECK(got.back() == palette::Message{palette::msg::SelectNext{}});
    QKeyEvent esc(QEvent::KeyPress, Qt::Key_Escape, Qt::NoModifier);
    QApplication::sendEvent(input, &esc);
    CHECK(got.back() == palette::Message{palette::msg::Close{}});
}

TEST_CASE("Qt key events become chords the keymap resolves") {
    app();
    const auto map = Keymap::defaults(palette::current_platform());
    QKeyEvent k(QEvent::KeyPress, Qt::Key_K, Qt::ControlModifier, QStringLiteral("\x0b"));
    const auto press = to_key_press(k);
    REQUIRE(press);
    const auto* id = map.resolve(*press, palette::Scope::Global);
    REQUIRE(id != nullptr);
    CHECK(id->as_str() == palette::ids::PALETTE_TOGGLE);
    QKeyEvent ctrl(QEvent::KeyPress, Qt::Key_Control, Qt::ControlModifier);
    CHECK(map.resolve(*to_key_press(ctrl), palette::Scope::Global) == nullptr);
}
