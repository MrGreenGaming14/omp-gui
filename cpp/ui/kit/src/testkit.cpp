#include "loa/ui/kit/testkit.hpp"

#include <QAbstractButton>
#include <QApplication>
#include <QKeyEvent>
#include <QWidget>

namespace loa::ui::kit::testkit {

void key(QWidget& target, int key, int modifiers, std::string_view text) {
    const auto mods = Qt::KeyboardModifiers(modifiers);
    const auto t = QString::fromUtf8(text.data(), static_cast<qsizetype>(text.size()));
    QKeyEvent press(QEvent::KeyPress, key, mods, t);
    QKeyEvent release(QEvent::KeyRelease, key, mods, t);
    QApplication::sendEvent(&target, &press);
    QApplication::sendEvent(&target, &release);
}

void type_char(QWidget& target, std::string_view c, int modifiers) {
    const auto s = QString::fromUtf8(c.data(), static_cast<qsizetype>(c.size()));
    const int k = s.isEmpty() ? 0 : s.toUpper().at(0).unicode();
    const bool command = (modifiers & (Qt::ControlModifier | Qt::MetaModifier)) != 0;
    key(target, k, modifiers, command ? std::string_view{} : c);
}

bool click_label(QWidget& root, std::string_view label) {
    const auto want = QString::fromUtf8(label.data(), static_cast<qsizetype>(label.size()));
    for (auto* b : root.findChildren<QAbstractButton*>()) {
        if ((b->text() == want || b->accessibleName() == want) && b->isEnabled()) {
            b->click();
            return true;
        }
    }
    return false;
}

} // namespace loa::ui::kit::testkit
