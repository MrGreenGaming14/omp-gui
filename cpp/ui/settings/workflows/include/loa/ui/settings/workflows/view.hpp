#pragma once

// Qt view of the workflows screen. Renders `const workflows::State&` and reports input through
// one `message` signal; never mutates the state.

#include <QWidget>

#include "loa/ui/settings/workflows/state.hpp"

namespace loa::ui::settings::workflows {

class WorkflowsView : public QWidget {
    Q_OBJECT

  public:
    // `titled` shows the screen's own title and Close (Rust `view`); without it only the list and
    // the editor (Rust `body`).
    explicit WorkflowsView(bool titled = true, QWidget* parent = nullptr);
    void render(const State& state);

  signals:
    void message(const loa::ui::settings::workflows::Message& m);

  private:
    bool titled_ = true;
    QWidget* content_ = nullptr;
};

} // namespace loa::ui::settings::workflows
