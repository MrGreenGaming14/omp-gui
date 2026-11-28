#pragma once

// The token layer: named roles, token sets, theme sources and loaders, the registry, and the
// active set that style recipes read. A future theme script only produces data through
// `ThemeSource`; widget code never changes. Port of `loa-ui-kit/src/theme`.

#include <array>
#include <cstddef>
#include <cstdint>
#include <expected>
#include <map>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/runtime/registry.hpp"
#include "loa/view/theme.hpp"

namespace loa::ui::kit::theme {

using ThemeRef = loa::view::theme::ThemeRef;
using Appearance = loa::view::theme::Appearance;
using Overrides = loa::view::theme::Overrides;

// An RGBA colour with components in 0..=1 (iced `Color`).
struct Color {
    float r = 0, g = 0, b = 0, a = 1;

    static constexpr Color transparent() { return {0, 0, 0, 0}; }
    // `iced::color!(0xrrggbb, a)`.
    static constexpr Color hex(std::uint32_t rgb, float alpha = 1.0F) {
        return {static_cast<float>((rgb >> 16) & 0xff) / 255.0F, static_cast<float>((rgb >> 8) & 0xff) / 255.0F,
                static_cast<float>(rgb & 0xff) / 255.0F, alpha};
    }
    static constexpr Color rgb8(std::uint8_t r, std::uint8_t g, std::uint8_t b) { return rgba8(r, g, b, 1.0F); }
    static constexpr Color rgba8(std::uint8_t r, std::uint8_t g, std::uint8_t b, float a) {
        return {static_cast<float>(r) / 255.0F, static_cast<float>(g) / 255.0F, static_cast<float>(b) / 255.0F, a};
    }
    [[nodiscard]] constexpr Color scale_alpha(float f) const { return {r, g, b, a * f}; }

    friend bool operator==(const Color&, const Color&) = default;
};

// The font family a text role uses.
enum class Font : std::uint8_t { Default, Monospace };

// ---- Roles ---------------------------------------------------------------------------------

enum class ColorRole : std::uint8_t {
    Ground, Side, Card, Raised, Hover, Selected, Line, LineStrong, Text, Text2, Text3, Accent, OnAccent, Ok, Warn,
    Danger, AddedBg, RemovedBg, CursorRing, Scrim, Shadow, Term0, Term1, Term2, Term3, Term4, Term5, Term6, Term7,
    Term8, Term9, Term10, Term11, Term12, Term13, Term14, Term15,
};
enum class TextRole : std::uint8_t { Title, Heading, Body, Label, Caption, Mono };
enum class SpaceRole : std::uint8_t { Xs, S, M, L, Xl, Gutter };
enum class RadiusRole : std::uint8_t { Pill, Chip, Row, Card, Dot };
enum class AlphaRole : std::uint8_t { Muted, Disabled, Tint, Hover };
enum class LayoutRole : std::uint8_t {
    ComposerWidth, NavWidth, ListWidth, PopupWidth, ContentWidth, LabelColumn, ComposerInset, MainWidth, MainHeight,
    WindowWidth, WindowHeight, DialogWidth, DetachWidth, DetachHeight, TallHeight, MinWidth, MinHeight,
};
// Light or dark; a source offers one or both.
enum class Variant : std::uint8_t { Light, Dark };
// Interaction state a `StyleSpec` applies to.
enum class StyleState : std::uint8_t { Base, Hover, Pressed, Selected, Disabled };

// Stable dotted names and the override prefix (`KIND`) of each role enum.
template <typename R> struct RoleNames;

#define LOA_KIT_ROLE_NAMES(Type, kind_, ...)                                                                     \
    template <> struct RoleNames<Type> {                                                                         \
        static constexpr std::string_view KIND = kind_;                                                          \
        static constexpr auto NAMES = std::to_array<std::string_view>({__VA_ARGS__});                            \
    };

LOA_KIT_ROLE_NAMES(ColorRole, "color", "ground", "side", "card", "raised", "hover", "selected", "line", "line.strong",
                   "text", "text.2", "text.3", "accent", "on.accent", "ok", "warn", "danger", "diff.added",
                   "diff.removed", "cursor.ring", "scrim", "shadow", "term.0", "term.1", "term.2", "term.3", "term.4",
                   "term.5", "term.6", "term.7", "term.8", "term.9", "term.10", "term.11", "term.12", "term.13",
                   "term.14", "term.15")
LOA_KIT_ROLE_NAMES(TextRole, "text", "title", "heading", "body", "label", "caption", "mono")
LOA_KIT_ROLE_NAMES(SpaceRole, "space", "xs", "s", "m", "l", "xl", "gutter")
LOA_KIT_ROLE_NAMES(RadiusRole, "radius", "pill", "chip", "row", "card", "dot")
LOA_KIT_ROLE_NAMES(AlphaRole, "alpha", "muted", "disabled", "tint", "hover")
LOA_KIT_ROLE_NAMES(LayoutRole, "layout", "composer.width", "nav.width", "list.width", "popup.width",
                   "content.width", "label.column", "composer.inset", "main.width", "main.height", "window.width",
                   "window.height", "dialog.width", "detach.width", "detach.height", "tall.height", "min.width",
                   "min.height")
LOA_KIT_ROLE_NAMES(Variant, "variant", "light", "dark")
LOA_KIT_ROLE_NAMES(StyleState, "state", "base", "hover", "pressed", "selected", "disabled")

#undef LOA_KIT_ROLE_NAMES

// A role enum: stable names plus the override prefix (Rust `Role`).
template <typename R>
concept Role = requires { RoleNames<R>::NAMES; RoleNames<R>::KIND; };

template <Role R> inline constexpr std::size_t COUNT = RoleNames<R>::NAMES.size();

template <Role R> [[nodiscard]] constexpr std::string_view name(R r) { return RoleNames<R>::NAMES[static_cast<std::size_t>(r)]; }

template <Role R> [[nodiscard]] constexpr std::size_t index(R r) { return static_cast<std::size_t>(r); }

template <Role R> [[nodiscard]] constexpr std::optional<R> parse(std::string_view s) {
    for (std::size_t i = 0; i < COUNT<R>; ++i)
        if (RoleNames<R>::NAMES[i] == s) return static_cast<R>(i);
    return std::nullopt;
}

// Every value of `R`, in declaration order.
template <Role R> [[nodiscard]] constexpr std::array<R, COUNT<R>> all() {
    std::array<R, COUNT<R>> out{};
    for (std::size_t i = 0; i < out.size(); ++i) out[i] = static_cast<R>(i);
    return out;
}

// The ANSI colour `n` (0..16) of the terminal.
[[nodiscard]] constexpr ColorRole term(std::size_t n) {
    return static_cast<ColorRole>(static_cast<std::size_t>(ColorRole::Term0) + (n < 15 ? n : 15));
}

// ---- Errors --------------------------------------------------------------------------------

// Why a theme or an override could not be used.
struct ThemeError {
    enum class Kind : std::uint8_t { UnknownTheme, UnknownKey, BadValue, NoVariant, Load };
    Kind kind = Kind::Load;
    std::string subject; // theme id, key, or the load message
    std::string detail;  // the bad-value reason
    Variant variant = Variant::Dark;

    static ThemeError unknown_theme(std::string id) { return {Kind::UnknownTheme, std::move(id), {}, {}}; }
    static ThemeError unknown_key(std::string key) { return {Kind::UnknownKey, std::move(key), {}, {}}; }
    static ThemeError bad_value(std::string key, std::string why) { return {Kind::BadValue, std::move(key), std::move(why), {}}; }
    static ThemeError no_variant(std::string id, Variant v) { return {Kind::NoVariant, std::move(id), {}, v}; }
    static ThemeError load(std::string message) { return {Kind::Load, std::move(message), {}, {}}; }

    // The Rust `Display` text.
    [[nodiscard]] std::string to_string() const;

    friend bool operator==(const ThemeError&, const ThemeError&) = default;
};

// ---- Tokens --------------------------------------------------------------------------------

// A colour reference resolved against the tokens at draw time.
struct Paint {
    struct RoleRef { ColorRole role; friend bool operator==(const RoleRef&, const RoleRef&) = default; };
    struct Alpha { ColorRole role; AlphaRole alpha; friend bool operator==(const Alpha&, const Alpha&) = default; };
    struct Scaled { ColorRole role; float alpha; friend bool operator==(const Scaled&, const Scaled&) = default; };
    struct Literal { Color color; friend bool operator==(const Literal&, const Literal&) = default; };
    std::variant<RoleRef, Alpha, Scaled, Literal> v;

    friend bool operator==(const Paint&, const Paint&) = default;
};

// One widget recipe: `component.part`, as in `button.ghost` or `container.card`.
struct StyleKey {
    std::string_view component;
    std::string_view part;

    friend bool operator==(const StyleKey&, const StyleKey&) = default;
    friend auto operator<=>(const StyleKey&, const StyleKey&) = default;
};

// The visual fields a recipe sets; `nullopt` inherits from the base state.
struct StyleSpec {
    std::optional<Paint> bg;
    std::optional<Paint> fg;
    std::optional<std::pair<Paint, float>> border;
    std::optional<RadiusRole> radius;

    friend bool operator==(const StyleSpec&, const StyleSpec&) = default;
};

// A recipe per `StyleState`; `Pressed` falls back to `Hover`.
struct StyleStates {
    std::array<std::optional<StyleSpec>, COUNT<StyleState>> states{};
    friend bool operator==(const StyleStates&, const StyleStates&) = default;
};

struct Border {
    Color color = Color::transparent();
    float width = 0;
    float radius = 0;
    friend bool operator==(const Border&, const Border&) = default;
};

// A recipe resolved to colours.
struct Look {
    std::optional<Color> bg;
    std::optional<Color> fg;
    Border border;
    friend bool operator==(const Look&, const Look&) = default;
};

// An override value: `#rrggbb[aa]`, a number, another role's name, or `role @ alpha`.
struct TokenValue {
    struct Ref { std::string name; friend bool operator==(const Ref&, const Ref&) = default; };
    struct Alpha { std::string base, alpha; friend bool operator==(const Alpha&, const Alpha&) = default; };
    std::variant<Color, float, Ref, Alpha> v;

    [[nodiscard]] static TokenValue parse(std::string_view s);
    friend bool operator==(const TokenValue&, const TokenValue&) = default;
};

// iced's six-colour palette, derived from the colour roles.
struct Palette {
    Color background, text, primary, success, warning, danger;
    friend bool operator==(const Palette&, const Palette&) = default;
};

// Every visual value of one theme variant.
struct TokenSet {
    std::string name;
    Variant variant = Variant::Dark;
    std::array<Color, COUNT<ColorRole>> colors{};
    std::array<std::pair<float, Font>, COUNT<TextRole>> text{};
    std::array<float, COUNT<SpaceRole>> space{};
    std::array<float, COUNT<RadiusRole>> radii{};
    std::array<float, COUNT<AlphaRole>> alphas{};
    std::array<float, COUNT<LayoutRole>> layout{};
    std::map<StyleKey, StyleStates> styles;

    [[nodiscard]] Color color(ColorRole r) const { return colors[index(r)]; }
    [[nodiscard]] float text_size(TextRole r) const { return text[index(r)].first; }
    [[nodiscard]] Font font(TextRole r) const { return text[index(r)].second; }
    [[nodiscard]] float spacing(SpaceRole r) const { return space[index(r)]; }
    [[nodiscard]] float radius(RadiusRole r) const { return radii[index(r)]; }
    [[nodiscard]] float alpha(AlphaRole r) const { return alphas[index(r)]; }
    [[nodiscard]] float layout_size(LayoutRole r) const { return layout[index(r)]; }
    [[nodiscard]] bool is_dark() const { return variant == Variant::Dark; }

    [[nodiscard]] Color paint(const Paint& p) const;
    [[nodiscard]] Palette palette() const;

    // The recipe `key` in `state`, layered over its base state.
    [[nodiscard]] Look look(StyleKey key, StyleState state) const;

    // Applies one override such as `color.accent` or `style.button.ghost.hover.bg`.
    std::expected<void, ThemeError> apply(std::string_view key, const TokenValue& v);

    // Applies a config `[theme]` table; nested tables join into dotted keys. Literal values go
    // first so references see them. Returns what failed.
    std::vector<ThemeError> apply_table(const toml::table& t);
    std::vector<ThemeError> apply_table(const Overrides& o) { return apply_table(o.table()); }

    friend bool operator==(const TokenSet&, const TokenSet&) = default;

  private:
    [[nodiscard]] std::optional<Paint> to_paint(const TokenValue& v) const;
    bool apply_style(std::string_view path, const TokenValue& v);
};

// ---- Sources and registry ------------------------------------------------------------------

// A theme: named token sets per variant. Builtins today; a script adapter later.
class ThemeSource {
  public:
    virtual ~ThemeSource() = default;
    [[nodiscard]] virtual std::string_view id() const = 0;
    [[nodiscard]] virtual std::string_view label() const = 0;
    [[nodiscard]] virtual std::span<const Variant> variants() const = 0;
    [[nodiscard]] virtual std::expected<TokenSet, ThemeError> tokens(Variant v) const = 0;
};

// Turns a ref the registry does not know (a file path, a script) into a source.
class ThemeLoader {
  public:
    virtual ~ThemeLoader() = default;
    [[nodiscard]] virtual bool can_load(const ThemeRef& r) const = 0;
    [[nodiscard]] virtual std::expected<std::shared_ptr<ThemeSource>, ThemeError> load(const ThemeRef& r) const = 0;
};

// A shipped theme: `system` offers both variants, `dark` and `light` one each.
class BuiltinTheme final : public ThemeSource {
  public:
    BuiltinTheme(std::string_view id, std::string_view label, std::span<const Variant> variants)
        : id_(id), label_(label), variants_(variants) {}

    // `system`, `dark`, `light`.
    [[nodiscard]] static std::span<const BuiltinTheme> all();

    [[nodiscard]] std::string_view id() const override { return id_; }
    [[nodiscard]] std::string_view label() const override { return label_; }
    [[nodiscard]] std::span<const Variant> variants() const override { return variants_; }
    [[nodiscard]] std::expected<TokenSet, ThemeError> tokens(Variant v) const override;

  private:
    std::string_view id_;
    std::string_view label_;
    std::span<const Variant> variants_;
};

// Named theme sources plus loaders for everything else.
class ThemeRegistry {
  public:
    // `system`, `dark` and `light`.
    [[nodiscard]] static ThemeRegistry builtin();

    // Rust `register` (a C++ keyword).
    void insert(std::shared_ptr<ThemeSource> s);
    void add_loader(std::unique_ptr<ThemeLoader> l);

    [[nodiscard]] std::expected<std::shared_ptr<ThemeSource>, ThemeError> resolve(const ThemeRef& r) const;

    // The tokens `a` asks for in `preferred` lightness, with its overrides applied. An unknown
    // or broken theme falls back to the builtin set; every failure is returned beside the tokens.
    [[nodiscard]] std::pair<TokenSet, std::vector<ThemeError>> tokens(const Appearance& a, Variant preferred) const;

    // `(id, label)` of every registered theme, sorted by id, for pickers.
    [[nodiscard]] std::vector<std::pair<std::string, std::string>> list() const;

  private:
    loa::runtime::Registry<ThemeSource> sources_;
    std::vector<std::shared_ptr<ThemeLoader>> loaders_;
};

// The variant of `source` to show: `preferred` when offered, else its first.
[[nodiscard]] Variant pick_variant(const ThemeSource& source, Variant preferred);

// ---- The active set ------------------------------------------------------------------------

// The shipped token set of `v`.
[[nodiscard]] std::shared_ptr<const TokenSet> builtin_tokens(Variant v);

// The active token set (builtin dark until `set_active`).
[[nodiscard]] std::shared_ptr<const TokenSet> tokens();

// Makes `t` the active set and returns the new generation.
std::uint64_t set_active(TokenSet t);

// Bumped by every `set_active`; caches compare it to know when to rebuild.
[[nodiscard]] std::uint64_t generation();

[[nodiscard]] float sp(SpaceRole r);
[[nodiscard]] float text_size(TextRole r);
[[nodiscard]] float width(LayoutRole r);

// WCAG contrast ratio of two colours (1..=21).
[[nodiscard]] float contrast(Color a, Color b);

} // namespace loa::ui::kit::theme

namespace loa::ui {
// The recipe's name for the token layer.
namespace theme = kit::theme;
} // namespace loa::ui
