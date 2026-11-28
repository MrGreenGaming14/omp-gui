#pragma once

// Qt view of the agents and background jobs panes and their `PanelPane` descriptors. Renders
// `const agents::State&` and reports input through one `message` signal.

#include <QWidget>

#include "loa/ui/panel/agents.hpp"
#include "loa/ui/panel/view.hpp"

class QScrollArea;

namespace loa::ui::panel {

inline constexpr PanelPane AGENTS_PANE{"agents", "Agents", loa::view::theme::Icon::Bot};
inline constexpr PanelPane JOBS_PANE{"jobs", "Background Jobs", loa::view::theme::Icon::Terminal};

[[nodiscard]] constexpr const PanelPane& pane(const agents::State& s) noexcept {
    return s.kind() == agents::PaneKind::Agents ? AGENTS_PANE : JOBS_PANE;
}

class AgentsView : public QWidget {
    Q_OBJECT

  public:
    explicit AgentsView(QWidget* parent = nullptr);
    void render(const agents::State& state);

  signals:
    void message(const loa::ui::panel::agents::Message& m);

  private:
    QScrollArea* scroll_ = nullptr;
};

} // namespace loa::ui::panel
