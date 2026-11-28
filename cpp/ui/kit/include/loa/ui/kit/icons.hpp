#pragma once

// Lucide line icons (ISC, see `LICENSE-lucide`; the SVGs under `icons/` are copied from
// `lucide-static`) compiled in as Qt resources and tinted by tone. Renderers are built once
// per icon, so drawing an icon never re-parses its SVG.

#include <array>
#include <string_view>

#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/theme.hpp"
#include "loa/view/theme.hpp"

class QIcon;
class QImage;
class QPixmap;

namespace loa::ui::kit {

using Icon = loa::view::theme::Icon;

inline constexpr std::size_t ICON_COUNT = static_cast<std::size_t>(Icon::X) + 1;

// Every icon in declaration order.
[[nodiscard]] const std::array<Icon, ICON_COUNT>& all_icons();
// The Lucide file stem (`chevron-down`).
[[nodiscard]] std::string_view icon_file(Icon i);
// The Qt resource path (`:/loa/icons/chevron-down.svg`).
[[nodiscard]] std::string_view icon_resource(Icon i);

// `icon` square at `size` logical pixels filled with `color` (premultiplied ARGB32).
[[nodiscard]] QImage render_icon(Icon icon, int size, theme::Color color);

// `icon` sized like `size` text, in `tone`.
[[nodiscard]] QPixmap tinted(Icon icon, theme::TextRole size, Tone tone);
// `icon` at `size` pixels in the text colour.
[[nodiscard]] QPixmap icon(Icon icon, float size);
// `icon` at `size` in the text colour at the muted alpha, for chevrons and row icons.
[[nodiscard]] QPixmap muted(Icon icon, float size);
// `icon` at `size` in a fixed colour.
[[nodiscard]] QPixmap colored(Icon icon, float size, theme::Color color);
// A `QIcon` for buttons: `tone` normally, the disabled alpha when disabled.
[[nodiscard]] QIcon qicon(Icon icon, theme::TextRole size, Tone tone);

} // namespace loa::ui::kit
