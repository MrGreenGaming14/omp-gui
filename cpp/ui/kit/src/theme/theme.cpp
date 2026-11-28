// The registry that resolves a config `ThemeRef`, and the active set style recipes read.

#include <algorithm>
#include <cmath>
#include <mutex>
#include <shared_mutex>

#include "builtin.hpp"
#include "loa/ui/kit/theme.hpp"

namespace loa::ui::kit::theme {
namespace {

struct Active {
    std::shared_mutex lock;
    std::uint64_t generation = 0;
    std::shared_ptr<const TokenSet> set = builtin_set(Variant::Dark);
};

Active& active() {
    static Active a;
    return a;
}

// A non-owning `shared_ptr` to a static builtin.
std::shared_ptr<ThemeSource> unowned(const BuiltinTheme& t) {
    return std::shared_ptr<ThemeSource>(std::shared_ptr<void>(), const_cast<BuiltinTheme*>(&t));
}

} // namespace

ThemeRegistry ThemeRegistry::builtin() {
    ThemeRegistry r;
    for (const auto& t : BuiltinTheme::all()) r.insert(unowned(t));
    return r;
}

void ThemeRegistry::insert(std::shared_ptr<ThemeSource> s) {
    std::string id(s->id());
    sources_.insert(std::move(id), std::move(s));
}

void ThemeRegistry::add_loader(std::unique_ptr<ThemeLoader> l) { loaders_.push_back(std::move(l)); }

std::expected<std::shared_ptr<ThemeSource>, ThemeError> ThemeRegistry::resolve(const ThemeRef& r) const {
    if (auto s = sources_.get(r.as_str())) return s;
    const auto it = std::ranges::find_if(loaders_, [&r](const auto& l) { return l->can_load(r); });
    if (it == loaders_.end()) return std::unexpected(ThemeError::unknown_theme(r.as_str()));
    return (*it)->load(r);
}

std::pair<TokenSet, std::vector<ThemeError>> ThemeRegistry::tokens(const Appearance& a, Variant preferred) const {
    auto loaded = resolve(a.theme).and_then([preferred](const std::shared_ptr<ThemeSource>& s) {
        return s->tokens(pick_variant(*s, preferred));
    });
    std::pair<TokenSet, std::vector<ThemeError>> out;
    if (loaded) {
        out.first = std::move(*loaded);
    } else {
        out.first = *builtin_set(preferred);
        out.second.push_back(std::move(loaded.error()));
    }
    auto errors = out.first.apply_table(a.overrides);
    std::ranges::move(errors, std::back_inserter(out.second));
    return out;
}

std::vector<std::pair<std::string, std::string>> ThemeRegistry::list() const {
    std::vector<std::pair<std::string, std::string>> out;
    for (const auto& [id, s] : sources_) out.emplace_back(id, std::string(s->label()));
    return out;
}

Variant pick_variant(const ThemeSource& source, Variant preferred) {
    const auto offered = source.variants();
    if (std::ranges::find(offered, preferred) != offered.end()) return preferred;
    return offered.empty() ? preferred : offered.front();
}

std::shared_ptr<const TokenSet> builtin_tokens(Variant v) { return builtin_set(v); }

std::shared_ptr<const TokenSet> tokens() {
    auto& a = active();
    std::shared_lock g(a.lock);
    return a.set;
}

std::uint64_t set_active(TokenSet t) {
    auto next = std::make_shared<const TokenSet>(std::move(t));
    auto& a = active();
    std::unique_lock g(a.lock);
    a.set = std::move(next);
    return ++a.generation;
}

std::uint64_t generation() {
    auto& a = active();
    std::shared_lock g(a.lock);
    return a.generation;
}

float sp(SpaceRole r) { return tokens()->spacing(r); }
float text_size(TextRole r) { return tokens()->text_size(r); }
float width(LayoutRole r) { return tokens()->layout_size(r); }

float contrast(Color a, Color b) {
    auto lum = [](Color c) {
        auto ch = [](float v) { return v <= 0.03928F ? v / 12.92F : std::pow((v + 0.055F) / 1.055F, 2.4F); };
        return 0.2126F * ch(c.r) + 0.7152F * ch(c.g) + 0.0722F * ch(c.b);
    };
    const float la = lum(a), lb = lum(b);
    return (std::max(la, lb) + 0.05F) / (std::min(la, lb) + 0.05F);
}

} // namespace loa::ui::kit::theme
