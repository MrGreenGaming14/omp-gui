// `TokenSet`: every visual value of one theme variant, plus the override parser that edits it
// by dotted key. The one kit TU that includes toml++.

#include <algorithm>
#include <charconv>
#include <sstream>

#include <toml++/toml.hpp>

#include "loa/ui/kit/theme.hpp"

namespace loa::ui::kit::theme {
namespace {

std::string_view trim(std::string_view s) {
    constexpr std::string_view ws = " \t\n\r\f\v";
    const auto b = s.find_first_not_of(ws);
    if (b == std::string_view::npos) return {};
    return s.substr(b, s.find_last_not_of(ws) - b + 1);
}

// Rust `str::parse::<f32>`: the whole string, an optional sign, `inf`/`nan` included.
std::optional<float> parse_f32(std::string_view s) {
    if (s.empty()) return std::nullopt;
    if (s.front() == '+') {
        s.remove_prefix(1);
        if (s.empty() || s.front() == '-' || s.front() == '+') return std::nullopt;
    }
    float v = 0;
    const auto [end, ec] = std::from_chars(s.data(), s.data() + s.size(), v);
    if (ec != std::errc{} || end != s.data() + s.size()) return std::nullopt;
    return v;
}

// `rrggbb` or `rrggbbaa`.
std::optional<Color> hex(std::string_view s) {
    const auto len = s.size();
    if (!s.empty() && s.front() == '+') s.remove_prefix(1);
    if (s.empty()) return std::nullopt;
    std::uint32_t v = 0;
    const auto [end, ec] = std::from_chars(s.data(), s.data() + s.size(), v, 16);
    if (ec != std::errc{} || end != s.data() + s.size()) return std::nullopt;
    auto b = [v](unsigned shift) { return static_cast<std::uint8_t>((v >> shift) & 0xff); };
    if (len == 6) return Color::rgb8(b(16), b(8), b(0));
    if (len == 8) return Color::rgba8(b(24), b(16), b(8), static_cast<float>(b(0)) / 255.0F);
    return std::nullopt;
}

void flatten(const std::string& prefix, const toml::table& t, std::vector<std::pair<std::string, const toml::node*>>& out) {
    for (const auto& [k, v] : t) {
        std::string key = prefix.empty() ? std::string(k.str()) : prefix + "." + std::string(k.str());
        if (const auto* inner = v.as_table())
            flatten(key, *inner, out);
        else
            out.emplace_back(std::move(key), &v);
    }
}

std::string display(const toml::node& n) {
    if (const auto* b = n.as_boolean()) return b->get() ? "true" : "false";
    std::ostringstream os;
    n.visit([&os](const auto& x) { os << x; });
    return os.str();
}

} // namespace

std::string ThemeError::to_string() const {
    switch (kind) {
    case Kind::UnknownTheme: return "unknown theme `" + subject + "`";
    case Kind::UnknownKey: return "unknown theme key `" + subject + "`";
    case Kind::BadValue: return "bad value for `" + subject + "`: " + detail;
    case Kind::NoVariant: return "theme `" + subject + "` does not offer the " + std::string(name(variant)) + " variant";
    case Kind::Load: return subject;
    }
    return subject;
}

TokenValue TokenValue::parse(std::string_view s) {
    s = trim(s);
    if (s.starts_with('#'))
        if (auto c = hex(s.substr(1))) return TokenValue{*c};
    if (auto n = parse_f32(s)) return TokenValue{*n};
    if (const auto at = s.find('@'); at != std::string_view::npos)
        return TokenValue{Alpha{std::string(trim(s.substr(0, at))), std::string(trim(s.substr(at + 1)))}};
    return TokenValue{Ref{std::string(s)}};
}

Color TokenSet::paint(const Paint& p) const {
    return std::visit(
        [this](const auto& x) -> Color {
            using T = std::decay_t<decltype(x)>;
            if constexpr (std::is_same_v<T, Paint::RoleRef>) return color(x.role);
            else if constexpr (std::is_same_v<T, Paint::Alpha>) return color(x.role).scale_alpha(alpha(x.alpha));
            else if constexpr (std::is_same_v<T, Paint::Scaled>) return color(x.role).scale_alpha(x.alpha);
            else return x.color;
        },
        p.v);
}

Palette TokenSet::palette() const {
    return {color(ColorRole::Ground), color(ColorRole::Text), color(ColorRole::Accent),
            color(ColorRole::Ok),     color(ColorRole::Warn), color(ColorRole::Danger)};
}

Look TokenSet::look(StyleKey key, StyleState state) const {
    StyleStates states;
    if (const auto it = styles.find(key); it != styles.end()) states = it->second;
    const StyleSpec base = states.states[0].value_or(StyleSpec{});
    auto over = states.states[index(state)];
    if (!over && state == StyleState::Pressed) over = states.states[index(StyleState::Hover)];
    StyleSpec s = base;
    if (over) {
        s.bg = over->bg ? over->bg : base.bg;
        s.fg = over->fg ? over->fg : base.fg;
        s.border = over->border ? over->border : base.border;
        s.radius = over->radius ? over->radius : base.radius;
    }
    Look l;
    if (s.bg) l.bg = paint(*s.bg);
    if (s.fg) l.fg = paint(*s.fg);
    if (s.border) l.border = Border{paint(s.border->first), s.border->second, 0};
    l.border.radius = s.radius ? radius(*s.radius) : 0.0F;
    return l;
}

std::optional<Paint> TokenSet::to_paint(const TokenValue& v) const {
    if (const auto* c = std::get_if<Color>(&v.v)) return Paint{Paint::Literal{*c}};
    if (const auto* r = std::get_if<TokenValue::Ref>(&v.v)) {
        const auto role = parse<ColorRole>(r->name);
        if (!role) return std::nullopt;
        return Paint{Paint::RoleRef{*role}};
    }
    if (const auto* a = std::get_if<TokenValue::Alpha>(&v.v)) {
        const auto role = parse<ColorRole>(a->base);
        if (!role) return std::nullopt;
        if (const auto n = parse_f32(a->alpha)) return Paint{Paint::Scaled{*role, *n}};
        const auto ar = parse<AlphaRole>(a->alpha);
        if (!ar) return std::nullopt;
        return Paint{Paint::Alpha{*role, *ar}};
    }
    return std::nullopt;
}

std::expected<void, ThemeError> TokenSet::apply(std::string_view key, const TokenValue& v) {
    auto bad = [key](std::string why) { return std::unexpected(ThemeError::bad_value(std::string(key), std::move(why))); };
    auto unknown = [key] { return std::unexpected(ThemeError::unknown_key(std::string(key))); };
    const auto dot = key.find('.');
    if (dot == std::string_view::npos) return unknown();
    const auto kind = key.substr(0, dot);
    const auto rest = key.substr(dot + 1);

    float* target = nullptr;
    auto slot = [&rest]<typename R>(R*, auto& arr) -> float* {
        const auto r = parse<R>(rest);
        return r ? &arr[index(*r)] : nullptr;
    };
    if (kind == "color") {
        const auto r = parse<ColorRole>(rest);
        if (!r) return unknown();
        const auto p = to_paint(v);
        if (!p) return bad("expected a colour");
        colors[index(*r)] = paint(*p);
        return {};
    }
    if (kind == "text") {
        if (const auto r = parse<TextRole>(rest)) target = &text[index(*r)].first;
    } else if (kind == "space") {
        target = slot(static_cast<SpaceRole*>(nullptr), space);
    } else if (kind == "radius") {
        target = slot(static_cast<RadiusRole*>(nullptr), radii);
    } else if (kind == "alpha") {
        target = slot(static_cast<AlphaRole*>(nullptr), alphas);
    } else if (kind == "layout") {
        target = slot(static_cast<LayoutRole*>(nullptr), layout);
    } else if (kind == "style") {
        if (!apply_style(rest, v)) return bad("unknown recipe or value");
        return {};
    }
    if (target == nullptr) return unknown();
    const auto* n = std::get_if<float>(&v.v);
    if (n == nullptr) return bad("expected a number");
    *target = *n;
    return {};
}

bool TokenSet::apply_style(std::string_view path, const TokenValue& v) {
    const auto f = path.rfind('.');
    if (f == std::string_view::npos) return false;
    const auto rest = path.substr(0, f);
    const auto field = path.substr(f + 1);
    const auto s = rest.rfind('.');
    if (s == std::string_view::npos) return false;
    const auto key_name = rest.substr(0, s);
    const auto state = parse<StyleState>(rest.substr(s + 1));
    if (!state) return false;
    const auto it = std::ranges::find_if(styles, [key_name](const auto& kv) {
        const auto& k = kv.first;
        return key_name.size() == k.component.size() + 1 + k.part.size() && key_name.starts_with(k.component) &&
               key_name[k.component.size()] == '.' && key_name.ends_with(k.part);
    });
    if (it == styles.end()) return false;
    const auto p = to_paint(v);
    std::optional<RadiusRole> radius;
    if (const auto* r = std::get_if<TokenValue::Ref>(&v.v)) radius = parse<RadiusRole>(r->name);
    auto& slot = it->second.states[index(*state)];
    if (!slot) slot = StyleSpec{};
    auto& spec = *slot;
    if (field == "bg") {
        if (!p) return false;
        spec.bg = p;
    } else if (field == "fg") {
        if (!p) return false;
        spec.fg = p;
    } else if (field == "border") {
        if (!p) return false;
        spec.border = std::pair{*p, spec.border ? spec.border->second : 1.0F};
    } else if (field == "radius") {
        if (!radius) return false;
        spec.radius = radius;
    } else {
        return false;
    }
    return true;
}

std::vector<ThemeError> TokenSet::apply_table(const toml::table& t) {
    std::vector<std::pair<std::string, const toml::node*>> flat;
    flatten("", t, flat);
    std::vector<ThemeError> errors;
    std::vector<std::pair<std::string, TokenValue>> values;
    for (auto& [k, node] : flat) {
        if (const auto* s = node->as_string())
            values.emplace_back(std::move(k), TokenValue::parse(s->get()));
        else if (const auto* i = node->as_integer())
            values.emplace_back(std::move(k), TokenValue{static_cast<float>(i->get())});
        else if (const auto* d = node->as_floating_point())
            values.emplace_back(std::move(k), TokenValue{static_cast<float>(d->get())});
        else
            errors.push_back(ThemeError::bad_value(std::move(k), display(*node)));
    }
    auto is_ref = [](const auto& kv) {
        return std::holds_alternative<TokenValue::Ref>(kv.second.v) || std::holds_alternative<TokenValue::Alpha>(kv.second.v);
    };
    std::ranges::stable_sort(values, {}, [&](const auto& kv) { return is_ref(kv); });
    for (const auto& [k, v] : values)
        if (auto r = apply(k, v); !r) errors.push_back(std::move(r.error()));
    return errors;
}

} // namespace loa::ui::kit::theme
