#pragma once

// Rewind plans between two states of one session: which live work to cancel
// or resume, and how to reach the target (drafts, else a fork). An id whose
// kind or parent differs between the two is treated as two different elements.

#include <cstdint>
#include <expected>
#include <string>
#include <variant>
#include <vector>

#include "loa/core/dom.hpp"
#include "loa/core/patch.hpp"

namespace loa::core::rewind {

namespace inexpressible {
// Ids are never reused, so a node only in the target can't be inserted again.
struct Added {
    NodeId node;
    friend bool operator==(const Added&, const Added&) = default;
};
// An `<ext>` attribute named like the reserved text name can't be `set`.
struct TextAttr {
    NodeId node;
    friend bool operator==(const TextAttr&, const TextAttr&) = default;
};
} // namespace inexpressible

// Why drafts can't move one DOM to the other.
using Inexpressible = std::variant<inexpressible::Added, inexpressible::TextAttr>;

// The Rust `Display` text.
[[nodiscard]] std::string to_string(const Inexpressible& e);

// Commit these drafts, in order, to the current session.
struct Drafts {
    std::vector<Draft> drafts;
    friend bool operator==(const Drafts&, const Drafts&) = default;
};
// Fork the session's journal at the target's `seq` and continue there.
struct Fork {
    std::uint64_t seq;
    Inexpressible why;
    friend bool operator==(const Fork&, const Fork&) = default;
};
// How a rewind moves the session DOM to the target state.
using Apply = std::variant<Drafts, Fork>;

// What the app does to rewind a session from its current DOM to a target DOM.
struct RewindPlan {
    // Live now and not live in the target: stop them, deepest first.
    std::vector<NodeId> cancel;
    // Live in the target and not live now: restart them, parents first.
    std::vector<NodeId> resume;
    Apply apply;
    friend bool operator==(const RewindPlan&, const RewindPlan&) = default;
};

// Plans a rewind from `current` to `target`, an earlier state of the same session.
[[nodiscard]] RewindPlan plan(const Dom& current, const Dom& target);

// Drafts that turn `old_dom` into `new_dom`: removes of the removed subtrees,
// then sets and appends on changed nodes, each in `old_dom`'s document order.
[[nodiscard]] std::expected<std::vector<Draft>, Inexpressible> drafts(const Dom& old_dom, const Dom& new_dom);

// True for work in progress a rewind has to stop or restart: running or
// waiting turns, streaming messages, pending or running calls, running jobs
// and waiting decisions.
[[nodiscard]] bool is_live(const Node& node);

} // namespace loa::core::rewind
