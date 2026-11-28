#pragma once

// Qt rendering of the token layer: colours, fonts, the application palette, and stylesheets
// for the `style.hpp` recipes. Qt types are forward-declared; include Qt in `.cpp` files.

#include <string>

#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/theme.hpp"

class QColor;
class QFont;
class QPalette;
class QWidget;

namespace loa::ui::kit::qt {

[[nodiscard]] QColor qcolor(theme::Color c);
// `rgba(r, g, b, a)` for stylesheets.
[[nodiscard]] std::string css(theme::Color c);

// The font of text role `r`: its size in pixels and family (monospace for `Mono`).
[[nodiscard]] QFont font(theme::TextRole r, const theme::TokenSet& t);
[[nodiscard]] QFont font(theme::TextRole r);

// A `QPalette` from the colour roles.
[[nodiscard]] QPalette palette(const theme::TokenSet& t);

// Stylesheet rules for `selector` in every state of the button recipe `k`.
[[nodiscard]] std::string button_qss(ButtonKind k, const std::string& selector, const theme::TokenSet& t);
// Stylesheet rules for `selector` drawn as surface `s`.
[[nodiscard]] std::string container_qss(Surface s, const std::string& selector, const theme::TokenSet& t);
// Application-wide rules: inputs, check boxes, combo boxes, scroll bars, tooltips, splitters.
[[nodiscard]] std::string app_qss(const theme::TokenSet& t);

// Applies `button_qss(k)` to `w` (scoped to its object name) and makes hovering repaint it.
void style_button(QWidget& w, ButtonKind k);
// Applies `container_qss(s)` to `w` (scoped to its object name).
void style_container(QWidget& w, Surface s);

} // namespace loa::ui::kit::qt
