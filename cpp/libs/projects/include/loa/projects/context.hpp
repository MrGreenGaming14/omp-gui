#pragma once

// Project context: shared instructions, the ideas notes when shared, pinned files and ranked digests, as
// markdown.

#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/projects/rank.hpp"
#include "loa/projects/settings.hpp"
#include "loa/projects/source.hpp"

namespace loa::projects {

struct ContextRequest {
    std::string_view project;
    // The chat itself, left out of its own context.
    std::optional<std::string_view> session;
    // The chat's first prompt or title.
    std::string_view query;
    // In estimated tokens. Instructions, shared ideas notes and pins always go in; digests get what is left.
    std::size_t budget_tokens = 0;
    // Unix milliseconds, for recency.
    std::int64_t now = 0;
};

struct PackedContext {
    std::string project;
    settings::ProjectSettings settings;
    // The ideas notes, when the project shares them with chats and they aren't blank.
    std::optional<std::string> ideas;
    // Best first.
    std::vector<Ranked> digests;
    friend bool operator==(const PackedContext&, const PackedContext&) = default;
};

// Ranks the visible sessions' digests and packs them with the settings into the budget. Shared ideas notes
// that can't be read are left out; the rest of the context still goes in.
[[nodiscard]] Result<PackedContext> build_context(const SessionSource& source, const Access& access,
                                                  const SettingsStore& store, const ContextRequest& request);

// The markdown section for one digest.
[[nodiscard]] std::string render_digest_section(const Candidate& candidate);

[[nodiscard]] std::string render_markdown(const PackedContext& context);

} // namespace loa::projects
