#pragma once

// Style recipes. Each reads its `StyleStates` or roles from the given (default: active) tokens
// at draw time, so overrides and future theme scripts restyle every widget. Port of
// `loa-ui-kit/src/style.rs`; the Qt stylesheet rendering of these looks is in `qss.hpp`.

#include <cstdint>
#include <optional>
#include <string_view>

#include "loa/ui/kit/theme.hpp"

namespace loa::ui::kit {

// Container looks; the name is the recipe's `container.<name>` key.
enum class Surface : std::uint8_t { Ground, Side, Card, Attention, Raised, Popup, Tooltip, Well, ErrorBox, Badge, Scrim };

// Text and icon colours by meaning.
enum class Tone : std::uint8_t { Normal, Muted, Faint, Ok, Warn, Danger, Accent, OnAccent };

[[nodiscard]] std::string_view name(Surface s);
[[nodiscard]] std::string_view name(Tone t);
[[nodiscard]] std::optional<Surface> parse_surface(std::string_view s);
[[nodiscard]] std::optional<Tone> parse_tone(std::string_view s);

// The colour role of `t`.
[[nodiscard]] theme::ColorRole role(Tone t);
// `t`'s colour in the active tokens.
[[nodiscard]] theme::Color color(Tone t);

// Button looks (Rust `ButtonKind`; `Row` and `Tab` carry `selected`/`active`).
struct ButtonKind {
    enum class Kind : std::uint8_t { Primary, Accent, Secondary, Ghost, Row, Tab, Flat, Danger };
    Kind kind = Kind::Secondary;
    bool on = false; // `Row { selected }` / `Tab { active }`

    static constexpr ButtonKind primary() { return {Kind::Primary}; }
    static constexpr ButtonKind accent() { return {Kind::Accent}; }
    static constexpr ButtonKind secondary() { return {Kind::Secondary}; }
    static constexpr ButtonKind ghost() { return {Kind::Ghost}; }
    static constexpr ButtonKind row(bool selected) { return {Kind::Row, selected}; }
    static constexpr ButtonKind tab(bool active) { return {Kind::Tab, active}; }
    static constexpr ButtonKind flat() { return {Kind::Flat}; }
    static constexpr ButtonKind danger() { return {Kind::Danger}; }

    [[nodiscard]] theme::StyleKey key() const;
    friend bool operator==(const ButtonKind&, const ButtonKind&) = default;
};

// iced `button::Status`.
enum class ButtonStatus : std::uint8_t { Active, Hovered, Pressed, Disabled };

// A resolved button look: background, text colour (defaults to `text`) and border.
struct ButtonStyle {
    std::optional<theme::Color> background;
    theme::Color text_color;
    theme::Border border;
    friend bool operator==(const ButtonStyle&, const ButtonStyle&) = default;
};

struct Shadow {
    theme::Color color = theme::Color::transparent();
    float dx = 0, dy = 0, blur = 0;
    friend bool operator==(const Shadow&, const Shadow&) = default;
};

struct ContainerStyle {
    std::optional<theme::Color> background;
    std::optional<theme::Color> text_color;
    theme::Border border;
    Shadow shadow;
    friend bool operator==(const ContainerStyle&, const ContainerStyle&) = default;
};

[[nodiscard]] theme::StyleState state_of(ButtonKind k, ButtonStatus s);
[[nodiscard]] ButtonStyle button(ButtonKind k, ButtonStatus s, const theme::TokenSet& t);
[[nodiscard]] ButtonStyle button(ButtonKind k, ButtonStatus s);
[[nodiscard]] ContainerStyle container(Surface s, const theme::TokenSet& t);
[[nodiscard]] ContainerStyle container(Surface s);

} // namespace loa::ui::kit
