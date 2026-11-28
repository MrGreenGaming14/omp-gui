#pragma once

// Qt view of the tools page and its `PanelPane` descriptor. Renders `const tools::State&` and
// reports input through one `message` signal; sections live in `tools::view::{Servers, Hosts, Peers}`.

#include <QWidget>

#include "loa/ui/panel/tools.hpp"
#include "loa/ui/panel/view.hpp"

class QLineEdit;

namespace loa::ui::panel {

inline constexpr PanelPane TOOLS_PANE{"tools", "Tools", loa::view::theme::Icon::Wrench};

[[nodiscard]] constexpr const PanelPane& pane(const tools::State&) noexcept { return TOOLS_PANE; }

class ToolsView : public QWidget {
    Q_OBJECT

  public:
    explicit ToolsView(QWidget* parent = nullptr);
    void render(const tools::State& state);

  signals:
    void message(const loa::ui::panel::tools::Message& m);

  private:
    // The filter input stays across renders so typing keeps focus; the rest is rebuilt.
    QLineEdit* search_ = nullptr;
    QWidget* tabs_ = nullptr;
    QWidget* controls_ = nullptr;
    QWidget* body_ = nullptr;
};

} // namespace loa::ui::panel
