#include "loa/app/view/theme.hpp"

#include <sstream>
#include <string>
#include <utility>

#include <toml++/toml.hpp>

namespace loa::app::view {

namespace {

namespace domain = ::loa::settings::app;
namespace shown = ::loa::view::theme;

// Both sides hold standard TOML tables, so each crosses as the TOML text the other parses.
::toml::table overrides_to_view(const domain::toml::Table& overrides) {
    const std::string text = domain::toml::write(overrides);
#if TOML_EXCEPTIONS
    return ::toml::parse(text);
#else
    ::toml::parse_result result = ::toml::parse(text);
    return std::move(result).table();
#endif
}

domain::toml::Table overrides_from_view(const ::toml::table& overrides) {
    std::ostringstream text;
    text << ::toml::toml_formatter{overrides};
    auto table = domain::toml::parse(text.str());
    return table ? std::move(*table) : domain::toml::Table{};
}

} // namespace

shown::ThemeRef theme_to_view(const domain::ThemeRef& theme) { return shown::ThemeRef{theme.name}; }

domain::ThemeRef theme_from_view(shown::ThemeRef theme) { return domain::ThemeRef{std::move(theme.value)}; }

shown::Appearance to_view(const domain::Appearance& appearance) {
    return shown::Appearance{theme_to_view(appearance.theme), shown::Overrides(overrides_to_view(appearance.overrides))};
}

domain::Appearance from_view(shown::Appearance appearance) {
    return domain::Appearance{theme_from_view(std::move(appearance.theme)), overrides_from_view(appearance.overrides.table())};
}

} // namespace loa::app::view
