#pragma once

// Qt view of the peers panel. Renders `const peers::State&` and reports input through one
// `message` signal; never mutates the state.

#include <QWidget>

#include "loa/ui/settings/peers/state.hpp"

namespace loa::ui::settings::peers {

class PeersView : public QWidget {
    Q_OBJECT

  public:
    explicit PeersView(QWidget* parent = nullptr);
    void render(const State& state);

  signals:
    void message(const loa::ui::settings::peers::Message& m);

  private:
    QWidget* content_ = nullptr;
};

} // namespace loa::ui::settings::peers
