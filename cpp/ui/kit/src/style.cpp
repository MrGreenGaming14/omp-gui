#include "loa/ui/kit/style.hpp"

#include <array>

namespace loa::ui::kit {
namespace {

constexpr std::array<std::string_view, 11> SURFACES = {"ground", "side",    "card", "attention", "raised", "popup",
                                                       "tooltip", "well", "error", "badge",     "scrim"};
constexpr std::array<std::string_view, 8> TONES = {"normal", "muted", "faint", "ok", "warn", "danger", "accent", "on.accent"};

template <typename E, std::size_t N> std::optional<E> find(const std::array<std::string_view, N>& names, std::string_view s) {
    for (std::size_t i = 0; i < N; ++i)
        if (names[i] == s) return static_cast<E>(i);
    return std::nullopt;
}

} // namespace

std::string_view name(Surface s) { return SURFACES[static_cast<std::size_t>(s)]; }
std::string_view name(Tone t) { return TONES[static_cast<std::size_t>(t)]; }
std::optional<Surface> parse_surface(std::string_view s) { return find<Surface>(SURFACES, s); }
std::optional<Tone> parse_tone(std::string_view s) { return find<Tone>(TONES, s); }

theme::ColorRole role(Tone t) {
    using C = theme::ColorRole;
    switch (t) {
    case Tone::Normal: return C::Text;
    case Tone::Muted: return C::Text2;
    case Tone::Faint: return C::Text3;
    case Tone::Ok: return C::Ok;
    case Tone::Warn: return C::Warn;
    case Tone::Danger: return C::Danger;
    case Tone::Accent: return C::Accent;
    case Tone::OnAccent: return C::OnAccent;
    }
    return C::Text;
}

theme::Color color(Tone t) { return theme::tokens()->color(role(t)); }

theme::StyleKey ButtonKind::key() const {
    static constexpr std::array<std::string_view, 8> parts = {"primary", "accent", "secondary", "ghost",
                                                              "row",     "tab",    "flat",      "danger"};
    return {"button", parts[static_cast<std::size_t>(kind)]};
}

theme::StyleState state_of(ButtonKind k, ButtonStatus s) {
    using S = theme::StyleState;
    if ((k.kind == ButtonKind::Kind::Row || k.kind == ButtonKind::Kind::Tab) && k.on) return S::Selected;
    switch (s) {
    case ButtonStatus::Hovered: return S::Hover;
    case ButtonStatus::Pressed: return S::Pressed;
    case ButtonStatus::Disabled: return S::Disabled;
    case ButtonStatus::Active: return S::Base;
    }
    return S::Base;
}

ButtonStyle button(ButtonKind k, ButtonStatus s, const theme::TokenSet& t) {
    const auto look = t.look(k.key(), state_of(k, s));
    return {look.bg, look.fg.value_or(t.color(theme::ColorRole::Text)), look.border};
}

ButtonStyle button(ButtonKind k, ButtonStatus s) { return button(k, s, *theme::tokens()); }

ContainerStyle container(Surface s, const theme::TokenSet& t) {
    const auto look = t.look({"container", name(s)}, theme::StyleState::Base);
    ContainerStyle out{look.bg, look.fg, look.border, {}};
    if (s == Surface::Popup) out.shadow = {t.color(theme::ColorRole::Shadow), 0, 16, 40};
    return out;
}

ContainerStyle container(Surface s) { return container(s, *theme::tokens()); }

} // namespace loa::ui::kit
