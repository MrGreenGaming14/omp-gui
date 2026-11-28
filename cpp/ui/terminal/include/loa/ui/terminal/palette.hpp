#pragma once

// Terminal colours: ANSI 0..16 from the kit `term.N` roles, the rest from ground/text/cursor/accent, plus
// xterm's cube and gray ramp. Port of `crates/loa-ui-terminal/src/palette.rs`.

#include <array>
#include <cstdint>

#include "loa/ui/kit/theme.hpp"
#include "loa/ui/terminal/emulator.hpp"

namespace loa::ui::terminal {

using Color = loa::ui::kit::theme::Color;

// iced `palette::mix`: `a` moved `factor` of the way to `b`, in linear light.
[[nodiscard]] Color mix(Color a, Color b, float factor);

// `ansi` holds the normal (0..8) and bright (8..16) ANSI colours.
struct TermPalette {
    std::array<Color, 16> ansi{};
    Color foreground, background, cursor, selection;

    // The colours of `tokens`; a new set after a theme change redraws.
    [[nodiscard]] static TermPalette from_tokens(const loa::ui::kit::theme::TokenSet& tokens);
    // A cell colour; `overrides` set by the program (OSC 4/10/11) win over the theme.
    [[nodiscard]] Color resolve(CellColor color, const Colors& overrides) const;
    [[nodiscard]] Color indexed(std::uint8_t i) const;

    friend bool operator==(const TermPalette&, const TermPalette&) = default;
};

} // namespace loa::ui::terminal
