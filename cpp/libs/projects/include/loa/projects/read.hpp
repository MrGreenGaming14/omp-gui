#pragma once

// `project_read`: the subtree behind a search hit or digest pointer, as indented text in byte pages.

#include <cstddef>
#include <optional>
#include <string>

#include "loa/core/dom.hpp"
#include "loa/projects/source.hpp"
#include "loa/runtime/text.hpp"

namespace loa::projects {

namespace read_pages {
// Page sizes in bytes.
inline constexpr runtime::text::Paginator PAGES{8'000, 32'000};
}

struct ReadRequest {
    std::string session;
    // Root of the subtree; none reads the whole session.
    std::optional<core::NodeId> node;
    // Byte offset into the rendered text.
    std::size_t offset = 0;
    // See `read_pages::PAGES`.
    std::size_t max_bytes = 0;
    friend bool operator==(const ReadRequest&, const ReadRequest&) = default;
};

struct ReadPage {
    std::string session;
    core::NodeId node;
    std::string text;
    std::size_t offset = 0;
    std::optional<std::size_t> next_offset;
    std::size_t total_bytes = 0;
    friend bool operator==(const ReadPage&, const ReadPage&) = default;
};

// A missing, other-project or denied session all give SessionNotFound.
[[nodiscard]] Result<ReadPage> read_subtree(const SessionSource& source, const Access& access, std::string_view project,
                                            const ReadRequest& request);

// `kind #id attr=value …` lines, each node's text one level deeper.
[[nodiscard]] std::string render_subtree(const core::Dom& dom, core::NodeId node);

} // namespace loa::projects
