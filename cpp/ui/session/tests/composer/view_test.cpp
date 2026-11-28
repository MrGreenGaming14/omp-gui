// The composer view renders the state and reports input as Messages (offscreen).

#include <QApplication>
#include <QKeyEvent>
#include <QLabel>
#include <QLineEdit>
#include <QPushButton>

#include "loa/ui/session/composer/view.hpp"
#include "support.hpp"

using namespace composer_test;

namespace {

QApplication& app() {
    static int argc = 1;
    static char name[] = "loa_ui_session_tests";
    static char* argv[] = {name, nullptr};
    static auto* a = new QApplication(argc, argv);
    return *a;
}

// A view wired to `state`: every message updates it and re-renders, as the app does.
struct Harness {
    State state;
    ComposerView view;
    std::vector<Message> seen;
    std::vector<Intent> intents;

    Harness() {
        QObject::connect(&view, &ComposerView::message, [this](const Message& m) {
            seen.push_back(m);
            auto [task, event] = state.update(m);
            if (event) intents.push_back(*event);
            view.render(state);
            view.apply(task);
        });
    }
    void render() {
        view.render(state);
        QApplication::processEvents();
    }
    void key(int key, Qt::KeyboardModifiers mods = Qt::NoModifier, const QString& text = {}) {
        QKeyEvent press(QEvent::KeyPress, key, mods, text);
        QApplication::sendEvent(view.editor(), &press);
    }
    void type(const QString& text) {
        for (const auto c : text) key(0, Qt::NoModifier, QString(c));
    }
};

template <class W> W* find(QWidget& root, const QString& text) {
    for (auto* w : root.findChildren<W*>())
        if ((w->objectName() == text || w->accessibleName() == text) && w->isVisibleTo(&root)) return w;
    return nullptr;
}

bool shows_text(QWidget& root, const QString& text) {
    for (auto* l : root.findChildren<QLabel*>())
        if (l->text().contains(text) && l->isVisibleTo(&root)) return true;
    return false;
}

} // namespace

TEST_CASE("typing, undo and send run through the state") {
    app();
    Harness h;
    h.render();
    h.view.show();
    h.type("hello world");
    CHECK(h.state.text() == "hello world");
    CHECK(h.view.editor()->toPlainText() == "hello world");
    h.key(Qt::Key_Z, Qt::ControlModifier, "z");
    CHECK(h.state.text() == "hello ");
    h.key(Qt::Key_Return, Qt::ShiftModifier);
    h.type("x");
    CHECK(h.view.editor()->toPlainText() == "hello \nx");
    auto* send = find<QPushButton>(h.view, "composer send");
    REQUIRE(send);
    CHECK(send->isEnabled());
    send->click();
    QApplication::processEvents();
    REQUIRE(h.intents.size() == 1);
    CHECK(std::get<intent::Submit>(h.intents[0]).prompt.text == "hello \nx");
    CHECK(h.view.editor()->toPlainText().isEmpty());
    CHECK_MESSAGE(find<QPushButton>(h.view, "composer stop"), "the stop button shows while the send starts");
}

TEST_CASE("the slash popup opens, filters and dismisses") {
    app();
    Harness h;
    h.state.set_commands({Command{"review", "review it"}, Command{"rewind", "go back"}, Command{"compact", "squash"}});
    h.render();
    h.view.show();
    h.type("/re");
    auto* popup = find<QWidget>(h.view, "composer popup");
    REQUIRE(popup);
    CHECK(shows_text(*popup, "/review"));
    CHECK(shows_text(*popup, "/rewind"));
    CHECK_FALSE(shows_text(*popup, "/compact"));
    h.key(Qt::Key_Escape);
    QApplication::processEvents();
    CHECK(find<QWidget>(h.view, "composer popup") == nullptr);
    CHECK(h.state.popup_kind() == std::nullopt);
}

TEST_CASE("queued messages, image cards and the working indicator render") {
    app();
    Harness h;
    h.type("first");
    h.render();
    (void)h.state.update(msg::Submit{});
    h.state.turn_started();
    h.state.set_activity(Activity::working("Reading the config\u2026", 0));
    type_text(h.state, "later");
    (void)h.state.update(msg::Submit{});
    (void)h.state.attach_image(pasted("/nonexistent/one.png", std::pair{10u, 20u}));
    h.render();
    h.view.show();
    QApplication::processEvents();
    CHECK(find<QWidget>(h.view, "composer queue"));
    auto* queued = find<QLineEdit>(h.view, "Queued message");
    REQUIRE(queued);
    CHECK(queued->text() == "later");
    CHECK(find<QWidget>(h.view, "Image #1"));
    CHECK(shows_text(h.view, "10x20"));
    CHECK(find<QWidget>(h.view, "composer working"));
    CHECK(shows_text(h.view, "esc to interrupt"));
    auto* remove = find<QPushButton>(h.view, "Remove image");
    REQUIRE(remove);
    remove->click();
    QApplication::processEvents();
    CHECK(h.state.images().front().shown == false);
    CHECK(find<QWidget>(h.view, "Image #1") == nullptr);
    auto* drop = find<QPushButton>(h.view, QString::fromUtf8(REMOVE_QUEUED_LABEL.data(), REMOVE_QUEUED_LABEL.size()));
    REQUIRE(drop);
    drop->click();
    QApplication::processEvents();
    CHECK(h.state.queue().empty());
    CHECK(find<QWidget>(h.view, "composer queue") == nullptr);
}

TEST_CASE("a picker chip opens its menu and picks") {
    app();
    Harness h;
    h.state.set_efforts({"low", "high"}, "low");
    h.state.set_access({AccessOption{"ask", "Ask", "asks first"}, AccessOption{"full", "Full", "never asks"}}, "ask");
    h.render();
    h.view.resize(800, 400);
    h.view.show();
    auto* chip = find<QPushButton>(h.view, "permissions picker");
    REQUIRE(chip);
    chip->click();
    QApplication::processEvents();
    CHECK(h.state.open_picker() == PickerId::Access);
    QPushButton* full = nullptr;
    for (auto* b : h.view.window()->findChildren<QPushButton*>())
        if (b->accessibleName().contains("Full") || b->text().contains("Full")) full = b;
    REQUIRE(full);
    full->click();
    QApplication::processEvents();
    REQUIRE_FALSE(h.intents.empty());
    CHECK(h.intents.back() == Intent{intent::AccessChanged{"full"}});
    CHECK(h.state.open_picker() == std::nullopt);
}
