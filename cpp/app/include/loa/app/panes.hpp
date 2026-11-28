#pragma once

// The registry of the right panel panes the app owns (port of `loa-app/src/app/panes.rs`):
// each is a `ui/panel` or `ui/terminal` pane state on the app, named and iconed by itself and
// drawn by the host's view of that state.

#include <array>
#include <string_view>

#include "loa/app/area.hpp"

namespace loa::app {

class App;

namespace panes {

// Which pane state on the app a registered pane draws.
enum class Kind : std::uint8_t { Diff, Files, Terminal };

// A pane the registry holds (Rust `trait Pane`).
struct Pane {
    Kind kind;
    // The pane's tab, listed in the panel from the start.
    PanelTab (*tab)(const App& app);
};

// The diff, files and terminal panes, in tab order.
extern const std::array<Pane, 3> REGISTRY;

// The registered pane whose tab is `id`; null when no built-in pane has it.
[[nodiscard]] const Pane* find(const App& app, std::string_view id);

} // namespace panes
} // namespace loa::app
