#pragma once

// Qt view of the updates screen. Renders `const updates::State&` and reports input through one
// `message` signal; never mutates the state.

#include <QWidget>

#include "loa/ui/settings/updates/state.hpp"

namespace loa::ui::settings::updates {

class UpdatesView : public QWidget {
    Q_OBJECT

  public:
    explicit UpdatesView(QWidget* parent = nullptr);
    void render(const State& state);

  signals:
    void message(const loa::ui::settings::updates::Message& m);

  private:
    QWidget* content_ = nullptr;
};

} // namespace loa::ui::settings::updates
