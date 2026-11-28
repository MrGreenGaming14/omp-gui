// The right panel panes the app owns (port of `loa-app/src/app/panes.rs`).

#include "loa/app/panes.hpp"

#include <algorithm>
#include <string>

#include "loa/app/app.hpp"
#include "loa/ui/panel/view.hpp"
#include "loa/ui/terminal/view.hpp"

namespace loa::app::panes {

namespace {

// `PanelTab::of(pane, true)`: a kit pane's id, title and icon, listed from the start.
template <const auto& P>
PanelTab of(const App&) {
    return {std::string(P.id), std::string(P.title), P.icon, true};
}

} // namespace

const std::array<Pane, 3> REGISTRY{
    Pane{Kind::Diff, &of<ui::panel::DIFF_PANE>},
    Pane{Kind::Files, &of<ui::panel::FILES_PANE>},
    Pane{Kind::Terminal, &of<ui::terminal::TERMINAL_PANE>},
};

const Pane* find(const App& app, std::string_view id) {
    auto it = std::ranges::find_if(REGISTRY, [&](const Pane& pane) { return pane.tab(app).id == id; });
    return it == REGISTRY.end() ? nullptr : &*it;
}

} // namespace loa::app::panes
