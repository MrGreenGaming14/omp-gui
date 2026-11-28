#pragma once

// What a session is at a glance: the summary lists and title bars show.

#include <cstdint>
#include <optional>
#include <string>

#include "loa/core/dom.hpp"
#include "loa/core/patch.hpp"

namespace loa::contracts {

struct SessionSummary {
    std::string id;
    std::string title;
    std::optional<std::string> project;
    // Public key of the node that runs the session.
    std::string node;
    bool pinned = false;
    // A decision in the session waits for an answer.
    bool waiting = false;
    // Unix ms of creation or of the latest `ended`. A turn that is still running does not move
    // the session.
    std::int64_t updated = 0;

    // Reads a `<session id node>` DOM: title attr or first user prompt line; `updated` is the
    // latest of `created` and every `ended`.
    [[nodiscard]] static std::optional<SessionSummary> from_dom(const loa::core::Dom& dom, bool pinned);

    // Brings the summary in line with `patch`, already applied to `dom`, and says whether it
    // changed. Walks the tree only for the rare patches that can lower `updated` or clear
    // `waiting`; streamed appends to replies cost a lookup. The result always equals `from_dom`.
    bool apply(const loa::core::Dom& dom, const loa::core::Patch& patch);

    friend bool operator==(const SessionSummary&, const SessionSummary&) = default;

  private:
    bool reload(const loa::core::Dom& dom);
    bool retitle(const loa::core::Dom& dom);
};

// The first line of `prompt`, cut to 60 characters plus `…`; empty without a non-blank line.
[[nodiscard]] std::optional<std::string> derived_title(std::string_view prompt);

} // namespace loa::contracts
