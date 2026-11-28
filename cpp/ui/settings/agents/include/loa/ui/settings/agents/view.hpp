#pragma once

// Qt view of the agents screen. Renders `const agents::State&` and reports input through one
// `message` signal; never mutates the state.

#include <QWidget>

#include "loa/ui/settings/agents/state.hpp"

namespace loa::ui::settings::agents {

class AgentsView : public QWidget {
    Q_OBJECT

  public:
    // `titled` shows the screen's own title and Close, for the full center (Rust `view`); without it
    // only the list and the form (Rust `body`), for a frame that has its own title.
    explicit AgentsView(bool titled = true, QWidget* parent = nullptr);
    void render(const State& state);

  signals:
    void message(const loa::ui::settings::agents::Message& m);

  private:
    bool titled_ = true;
    QWidget* content_ = nullptr;
};

} // namespace loa::ui::settings::agents
