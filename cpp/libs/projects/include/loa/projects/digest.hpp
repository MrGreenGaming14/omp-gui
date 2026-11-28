#pragma once

// The `<digest>` element as typed data. A session keeps at most one, under its `<session>`. It records the
// sequence it was built from and how many patches its own write took, and is stale when the session has
// patches beyond those or `stale` is set.

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/core/dom.hpp"
#include "loa/core/patch.hpp"
#include "loa/settings/projects.hpp"

namespace loa::projects {

// Extension attribute: patches the digest's own write committed (old removed, new inserted).
inline constexpr std::string_view OWN_PATCHES = "x-own_patches";

// A digest read from a session DOM.
struct Digest {
    core::NodeId node;
    settings::DigestContent content;
    std::uint64_t source_seq = 0;
    // Flagged stale, or the session moved on.
    bool stale = false;
    std::optional<std::string> model;
    friend bool operator==(const Digest&, const Digest&) = default;
};

[[nodiscard]] std::optional<Digest> digest_of(const core::Dom& dom);
[[nodiscard]] settings::DigestState digest_state(const core::Dom& dom);

// The drafts that replace the digest with `content` built at `source_seq`; commit them in order, back to
// back. Empty without a `<session>` root.
[[nodiscard]] std::vector<core::Draft> write_drafts(const core::Dom& dom, const settings::DigestContent& content,
                                                    std::uint64_t source_seq, std::optional<std::string_view> model,
                                                    std::int64_t created);

// Flags the digest stale (after a failed rebuild), unless it already is.
[[nodiscard]] std::optional<core::Draft> mark_stale_draft(const core::Dom& dom);

} // namespace loa::projects
