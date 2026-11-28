#include "loa/projects/search.hpp"

#include <algorithm>
#include <charconv>
#include <cmath>

#include "loa/core/schema.hpp"
#include "loa/projects/settings.hpp"
#include "loa/projects/text.hpp"

namespace loa::projects {

namespace attr = core::attr;
using core::ElementKind;
using core::Node;
using core::Value;
using runtime::text::ceil_boundary;
using runtime::text::clip;
using runtime::text::floor_boundary;

namespace {

// Bytes of context kept before and after a match.
constexpr std::size_t BEFORE = 60;
constexpr std::size_t AFTER = 120;

// A string value, then any string list items.
std::vector<std::string_view> strings(const Value& value) {
    std::vector<std::string_view> out;
    if (auto s = value.as_str()) out.push_back(*s);
    for (auto s : value.str_items()) out.push_back(s);
    return out;
}

// Lowercased text that maps each byte back to its offset in the original.
struct Folded {
    std::string lower;
    std::vector<std::size_t> origin;

    explicit Folded(std::string_view s) {
        lower.reserve(s.size());
        origin.reserve(s.size() + 1);
        for (std::size_t i = 0; i < s.size();) {
            const std::size_t at = i;
            (void)text::decode(s, i);
            lower += text::to_lowercase(s.substr(at, i - at));
            origin.resize(lower.size(), at);
        }
        origin.push_back(s.size());
    }

    [[nodiscard]] std::size_t at(std::size_t i) const { return origin[std::min(i, origin.size() - 1)]; }
};

// `s[start..end]` with context around it, on one line.
std::string window(std::string_view s, std::size_t start, std::size_t end) {
    const auto from = floor_boundary(s, start > BEFORE ? start - BEFORE : 0);
    const auto to = ceil_boundary(s, end + AFTER < end ? s.size() : end + AFTER);
    std::string out = from > 0 ? "…" : "";
    out += text::one_line(s.substr(from, to - from));
    if (to < s.size()) out += "…";
    return out;
}

std::string_view trim_end(std::string_view s) {
    auto full = text::trim(s);
    if (full.empty()) return {};
    return s.substr(0, static_cast<std::size_t>(full.data() - s.data()) + full.size());
}

// A short label for a node in a hit path.
std::string label(const Node& node) {
    std::optional<std::string_view> detail;
    switch (node.kind) {
        case ElementKind::Session:
            detail = node.str_attr(attr::TITLE);
            if (!detail) detail = node.str_attr(attr::ID);
            break;
        case ElementKind::Message: detail = node.str_attr(attr::ROLE); break;
        case ElementKind::Call: detail = node.str_attr(attr::TOOL); break;
        case ElementKind::Decision:
        case ElementKind::Job: detail = node.str_attr(attr::KIND); break;
        default: break;
    }
    const std::string kind(core::as_str(node.kind));
    if (detail) return kind + " " + text::one_line(clip(*detail, 60)) + " #" + core::to_string(node.id);
    return kind + " #" + core::to_string(node.id);
}

class Matcher {
  public:
    static Result<Matcher> make(const SearchQuery& query) {
        Matcher m;
        const auto txt = query.text ? text::trim(*query.text) : std::string_view{};
        if (!txt.empty()) {
            if (query.mode == TextMode::Phrase) m.needles_.push_back(text::to_lowercase(txt));
            else
                for (auto w : text::split_whitespace(txt)) m.needles_.push_back(text::to_lowercase(w));
        }
        if (m.needles_.empty() && !query.kind && query.attrs.empty())
            return std::unexpected(Error::arguments("give a text query, an element type or an attribute"));
        m.kind_ = query.kind;
        for (const auto& a : query.attrs)
            m.attrs_.emplace_back(a.name, a.value ? std::optional<std::string>(text::to_lowercase(*a.value)) : std::nullopt);
        return m;
    }

    // The snippet when `node` matches.
    [[nodiscard]] std::optional<std::string> matches(const core::Dom& dom, const Node& node) const {
        if (kind_ && *kind_ != node.kind) return std::nullopt;
        std::string prefix;
        for (const auto& [name, want] : attrs_) {
            const auto* value = node.attr(name);
            if (!value) return std::nullopt;
            if (want && std::ranges::none_of(strings(*value), [&](std::string_view v) {
                    return text::to_lowercase(v).find(*want) != std::string::npos;
                }))
                return std::nullopt;
            prefix += name + "=" + render_value(*value) + " ";
        }
        if (needles_.empty()) {
            auto shown = text::trim(node.text);
            if (shown.empty()) shown = node.str_attr(attr::TITLE).value_or("");
            return std::string(trim_end(prefix + text::one_line(clip(shown, BEFORE + AFTER))));
        }
        // An element filter widens the text scope to the subtree.
        std::vector<const Node*> scope;
        if (kind_)
            for (const auto& n : dom.descendants(node.id)) scope.push_back(&n);
        else
            scope.push_back(&node);
        std::vector<bool> found(needles_.size(), false);
        std::optional<std::string> snippet;
        const auto check = [&](std::string_view field) -> bool {
            const Folded folded(field);
            for (std::size_t i = 0; i < needles_.size(); ++i) {
                const auto at = folded.lower.find(needles_[i]);
                if (at == std::string::npos) continue;
                found[i] = true;
                if (!snippet) snippet = window(field, folded.at(at), folded.at(at + needles_[i].size()));
            }
            return std::ranges::all_of(found, [](bool f) { return f; });
        };
        for (const auto* n : scope) {
            if (check(n->text)) return prefix + snippet.value_or("");
            for (const auto& [_, value] : n->attrs)
                for (auto s : strings(value))
                    if (check(s)) return prefix + snippet.value_or("");
        }
        return std::nullopt;
    }

  private:
    std::vector<std::string> needles_;
    std::optional<ElementKind> kind_;
    std::vector<std::pair<std::string, std::optional<std::string>>> attrs_;
};

} // namespace

std::string render_value(const Value& value) {
    return std::visit(
        [](const auto& v) -> std::string {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, bool>) {
                return v ? "true" : "false";
            } else if constexpr (std::is_same_v<T, std::int64_t>) {
                return std::to_string(v);
            } else if constexpr (std::is_same_v<T, double>) {
                if (std::isnan(v)) return "NaN";
                if (std::isinf(v)) return v > 0 ? "inf" : "-inf";
                char buf[400];
                auto [end, ec] = std::to_chars(buf, buf + sizeof buf, v, std::chars_format::fixed);
                return ec == std::errc{} ? std::string(buf, end) : std::to_string(v);
            } else if constexpr (std::is_same_v<T, std::string>) {
                bool ws = v.empty();
                for (std::size_t i = 0; !ws && i < v.size();) ws = text::is_whitespace(text::decode(v, i));
                return ws ? text::debug_str(v) : v;
            } else if constexpr (std::is_same_v<T, Value::Bytes>) {
                return "<" + std::to_string(v.size()) + " bytes>";
            } else {
                std::string out = "[";
                for (std::size_t i = 0; i < v.size(); ++i) {
                    if (i) out += ", ";
                    out += render_value(v[i]);
                }
                return out + "]";
            }
        },
        value.variant());
}

Result<SearchPage> search(const SessionSource& source, const Access& access, std::string_view project,
                          const SearchQuery& query) {
    auto matcher = Matcher::make(query);
    if (!matcher) return std::unexpected(matcher.error());
    std::size_t skip = 0;
    if (query.cursor) {
        auto n = text::parse_usize(*query.cursor);
        if (!n) return std::unexpected(Error::arguments("bad cursor `" + *query.cursor + "`"));
        skip = *n;
    }
    const auto limit = search_pages::PAGES.size(query.limit);
    SearchPage page;
    auto visible = visible_sessions(source, access, project);
    if (!visible) return std::unexpected(visible.error());
    for (const auto& info : *visible) {
        const bool wanted = !query.sessions || std::ranges::find(*query.sessions, info.id) != query.sessions->end();
        if (!wanted || query.skip_session == info.id) continue;
        auto dom = source.load(info.id);
        if (!dom) return std::unexpected(dom.error());
        if (!*dom) continue;
        if (!query.include_excluded && is_excluded(**dom)) continue;
        for (const auto& node : (*dom)->iter()) {
            auto snippet = matcher->matches(**dom, node);
            if (!snippet) continue;
            ++page.total;
            if (page.total > skip && page.hits.size() < limit) {
                auto ancestors = (*dom)->ancestors(node.id);
                std::string path;
                for (auto it = ancestors.rbegin(); it != ancestors.rend(); ++it) {
                    if (!path.empty()) path += " > ";
                    path += label(**it);
                }
                page.hits.push_back(SearchHit{info.id, info.title, node.id, node.kind, std::move(*snippet), std::move(path)});
            }
        }
    }
    const auto shown = skip + page.hits.size();
    if (shown < page.total) page.next_cursor = std::to_string(shown);
    return page;
}

} // namespace loa::projects
