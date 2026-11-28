#pragma once

// The terminal grid's geometry and the input its canvas reports. Port of the plain parts of
// `crates/loa-ui-terminal/src/canvas.rs`; the Qt widget is `Canvas` in `view.hpp`.

#include <optional>
#include <string>
#include <utility>
#include <variant>

#include "loa/ui/terminal/emulator.hpp"
#include "loa/ui/terminal/input.hpp"

namespace loa::ui::terminal {

// Monospaced font size and cell geometry of the grid, in pixels.
struct Metrics {
    float font_size = 14;
    float cell_width = 8;
    float cell_height = 16;

    // The grid that fits in `width` × `height`, at least 2 by 1.
    [[nodiscard]] GridSize grid_for(float width, float height) const;
    // The cell under a canvas position and which half of it; outside positions clamp.
    [[nodiscard]] std::pair<ViewportCell, Side> cell_at(float x, float y, GridSize grid) const;

    friend bool operator==(const Metrics&, const Metrics&) = default;
};

// Canvas input for the state: `Scroll` lines are positive up; `SelectStart` is simple, semantic (2 clicks) or
// lines (3).
namespace canvas_event {
struct Resized {
    GridSize grid;
};
struct KeyPress {
    terminal::Key key;
    Modifiers modifiers;
    std::optional<std::string> text;
};
struct Scroll {
    int lines = 0;
};
struct SelectStart {
    ViewportCell cell;
    Side side = Side::Left;
    SelectionType kind = SelectionType::Simple;
};
struct SelectUpdate {
    ViewportCell cell;
    Side side = Side::Left;
};
struct SelectEnd {};
} // namespace canvas_event

using CanvasEvent = std::variant<canvas_event::Resized, canvas_event::KeyPress, canvas_event::Scroll, canvas_event::SelectStart,
                                 canvas_event::SelectUpdate, canvas_event::SelectEnd>;

} // namespace loa::ui::terminal
