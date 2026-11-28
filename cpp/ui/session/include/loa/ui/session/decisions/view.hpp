#pragma once

// Qt views of the decision cards and the unattended settings. Each renders its state and reports
// input through one `message` signal; none mutates its state.

#include <QWidget>

#include "loa/ui/session/decisions/state.hpp"
#include "loa/ui/session/decisions/unattended.hpp"

class QKeyEvent;
class QVBoxLayout;

namespace loa::ui::session::decisions {

// The view's key press as a `KeyPress` for `shortcut`.
[[nodiscard]] KeyPress key_press(const QKeyEvent& event);

// One card. `focused` draws it as the card taking shortcuts.
class CardView : public QWidget {
    Q_OBJECT

  public:
    explicit CardView(QWidget* parent = nullptr);
    void render(const Card& card, const State& state, bool focused);

  signals:
    void message(const loa::ui::session::decisions::Message& m);

  private:
    QVBoxLayout* layout_ = nullptr;
    QWidget* body_ = nullptr;
};

// Every card of one session in document order.
class DecisionsView : public QWidget {
    Q_OBJECT

  public:
    explicit DecisionsView(QWidget* parent = nullptr);
    void render(const State& state);

  signals:
    void message(const loa::ui::session::decisions::Message& m);

  private:
    QVBoxLayout* layout_ = nullptr;
};

// The unattended settings section: on/off, then per kind the default answer and its timeout.
class UnattendedSettingsView : public QWidget {
    Q_OBJECT

  public:
    explicit UnattendedSettingsView(QWidget* parent = nullptr);
    void render(const Unattended& policy);

  signals:
    void message(const loa::ui::session::decisions::unattended::Message& m);

  private:
    QVBoxLayout* layout_ = nullptr;
    QWidget* body_ = nullptr;
};

} // namespace loa::ui::session::decisions
