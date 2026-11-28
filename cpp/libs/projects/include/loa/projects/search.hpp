#pragma once

// `project_search`: case insensitive text and element search over a project's sessions. Without an element
// filter a node matches on its own text and string attributes; with one, a node of that kind matches when
// the text appears anywhere in its subtree. Visible sessions are searched newest first, excluded ones only
// when asked.

#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <vector>

#include "loa/core/schema.hpp"
#include "loa/core/value.hpp"
#include "loa/projects/source.hpp"
#include "loa/runtime/text.hpp"

namespace loa::projects {

namespace search_pages {
// Hits per page.
inline constexpr runtime::text::Paginator PAGES{20, 100};
}

// How a text query matches.
enum class TextMode : std::uint8_t {
    // The whole query as one substring.
    Phrase,
    // Every whitespace separated word, anywhere.
    AllWords,
};

// An attribute the element must have; `value` matches part of it (any list item).
struct AttrFilter {
    std::string name;
    std::optional<std::string> value;
    friend bool operator==(const AttrFilter&, const AttrFilter&) = default;
};

struct SearchQuery {
    std::optional<std::string> text;
    TextMode mode = TextMode::Phrase;
    std::optional<core::ElementKind> kind;
    std::vector<AttrFilter> attrs;
    std::optional<std::vector<std::string>> sessions;
    // The calling chat, which already has its own record.
    std::optional<std::string> skip_session;
    bool include_excluded = false;
    // See `search_pages::PAGES`.
    std::size_t limit = 0;
    std::optional<std::string> cursor;
    friend bool operator==(const SearchQuery&, const SearchQuery&) = default;
};

struct SearchHit {
    std::string session;
    std::optional<std::string> session_title;
    core::NodeId node;
    core::ElementKind kind;
    std::string snippet;
    // Ancestor labels from the `<session>` down to the hit's parent, joined by ` > `.
    std::string path;
    friend bool operator==(const SearchHit&, const SearchHit&) = default;
};

struct SearchPage {
    std::vector<SearchHit> hits;
    // Hits across every visible session.
    std::size_t total = 0;
    std::optional<std::string> next_cursor;
    friend bool operator==(const SearchPage&, const SearchPage&) = default;
};

[[nodiscard]] Result<SearchPage> search(const SessionSource& source, const Access& access, std::string_view project,
                                        const SearchQuery& query);

// An attribute value as `project_read` and snippets show it.
[[nodiscard]] std::string render_value(const core::Value& value);

} // namespace loa::projects
