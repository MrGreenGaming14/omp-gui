#pragma once

// The active theme: the saved `Appearance` resolved via `ThemeRegistry` against the system
// mode. Port of `loa-ui-shell/src/theme.rs`; the kit tokens are re-exported as
// `loa::ui::shell::theme`.

#include <cstdint>
#include <memory>
#include <string>
#include <utility>
#include <vector>

#include "loa/ui/kit/theme.hpp"
#include "loa/view/theme.hpp"

namespace loa::ui::shell::theme {

using namespace loa::ui::kit::theme;

// iced `theme::Mode`: what the system reports; `None` when it does not say.
enum class Mode : std::uint8_t { None, Light, Dark };

class State {
  public:
    explicit State(loa::view::theme::Appearance appearance);
    // Resolves through `registry`, which may carry extra sources and loaders.
    State(ThemeRegistry registry, loa::view::theme::Appearance appearance);

    [[nodiscard]] const loa::view::theme::Appearance& appearance() const { return appearance_; }
    [[nodiscard]] const loa::view::theme::ThemeRef& preference() const { return appearance_.theme; }
    [[nodiscard]] std::vector<std::pair<std::string, std::string>> themes() const { return registry_->list(); }
    [[nodiscard]] const std::vector<ThemeError>& errors() const { return errors_; }
    // An unknown system mode counts as dark.
    [[nodiscard]] Variant variant() const { return variant_; }
    // Picks a theme; returns the new token generation.
    std::uint64_t set_preference(loa::view::theme::ThemeRef theme);
    // Replaces the whole appearance, overrides included; returns the new token generation.
    std::uint64_t set_appearance(loa::view::theme::Appearance appearance);
    std::uint64_t set_system_mode(Mode mode);

  private:
    std::uint64_t refresh();

    std::shared_ptr<const ThemeRegistry> registry_;
    loa::view::theme::Appearance appearance_;
    Mode system_ = Mode::None;
    Variant variant_ = Variant::Dark;
    std::vector<ThemeError> errors_;
};

} // namespace loa::ui::shell::theme
