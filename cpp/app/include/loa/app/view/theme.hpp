#pragma once

// Appearance between the config schema and `loa::view::theme`.

#include "loa/settings/app/schema.hpp"
#include "loa/view/theme.hpp"

namespace loa::app::view {

[[nodiscard]] ::loa::view::theme::ThemeRef theme_to_view(const ::loa::settings::app::ThemeRef& theme);
[[nodiscard]] ::loa::settings::app::ThemeRef theme_from_view(::loa::view::theme::ThemeRef theme);
[[nodiscard]] ::loa::view::theme::Appearance to_view(const ::loa::settings::app::Appearance& appearance);
[[nodiscard]] ::loa::settings::app::Appearance from_view(::loa::view::theme::Appearance appearance);

} // namespace loa::app::view
