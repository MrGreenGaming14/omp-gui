#pragma once

// The Surfaces tab's provider styles (`SurfaceKindStyle`) and the document page palette, read
// from kit tokens. Port of `crates/loa-ui-surface/src/theme.rs`; the widget recipes live in
// the view.

#include <string_view>

#include "loa/view/surface.hpp"
#include "loa/view/theme.hpp"

namespace loa::ui::surface {

// Provider names of the built-in surface servers.
inline constexpr std::string_view BROWSER = "browser";
inline constexpr std::string_view VM = "vm";
inline constexpr std::string_view MCP = "mcp";

// How surfaces of one provider are drawn and titled; looked up by provider name, so a theme can
// remap it.
struct SurfaceKindStyle {
    loa::view::theme::Icon icon = loa::view::theme::Icon::Square;
    // The launcher section that opens this kind.
    std::string_view title;

    // The style of `provider`'s surfaces; providers without one (MCP servers) go by `mode`.
    [[nodiscard]] static SurfaceKindStyle of(std::string_view provider, loa::view::surface::SurfaceMode mode);

    friend bool operator==(const SurfaceKindStyle&, const SurfaceKindStyle&) = default;
};

// How document pages are drawn: in the light token set in either theme, since documents are
// paper like the PDF pages beside them.
[[nodiscard]] loa::view::surface::port::DocumentPalette document_palette();

} // namespace loa::ui::surface
