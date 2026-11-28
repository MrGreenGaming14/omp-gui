#include "loa/ui/shell/theme.hpp"

namespace loa::ui::shell::theme {

State::State(loa::view::theme::Appearance appearance) : State(ThemeRegistry::builtin(), std::move(appearance)) {}

State::State(ThemeRegistry registry, loa::view::theme::Appearance appearance)
    : registry_(std::make_shared<const ThemeRegistry>(std::move(registry))), appearance_(std::move(appearance)) {
    refresh();
}

std::uint64_t State::set_preference(loa::view::theme::ThemeRef theme) {
    appearance_.theme = std::move(theme);
    return refresh();
}

std::uint64_t State::set_appearance(loa::view::theme::Appearance appearance) {
    appearance_ = std::move(appearance);
    return refresh();
}

std::uint64_t State::set_system_mode(Mode mode) {
    system_ = mode;
    return refresh();
}

std::uint64_t State::refresh() {
    const auto preferred = system_ == Mode::Light ? Variant::Light : Variant::Dark;
    auto [tokens, errors] = registry_->tokens(appearance_, preferred);
    variant_ = tokens.variant;
    errors_ = std::move(errors);
    return set_active(std::move(tokens));
}

} // namespace loa::ui::shell::theme
